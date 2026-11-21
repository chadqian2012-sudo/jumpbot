#include <opencv2/core.hpp>
#include <opencv2/imgcodecs.hpp>
#include <opencv2/imgproc.hpp>
#include <algorithm>
#include <array>
#include <cmath>
#include <memory>
#include <vector>

// OpenCV owns matrix storage only within this translation unit. All output
// buffers passed to Rust are copied synchronously before the callback returns.
using Pixels = void (*)(void*, int, int, const unsigned char*);
extern "C" int jb_decode(const unsigned char* bytes, size_t count, void* context, Pixels copy) noexcept {
    try {
        if (!count || count > INT_MAX) return 0;
        auto image = cv::imdecode(cv::Mat(1, (int)count, CV_8U, const_cast<unsigned char*>(bytes)), cv::IMREAD_COLOR);
        if (image.empty()) return 0;
        copy(context, image.cols, image.rows, image.ptr()); return 1;
    } catch (...) { return 0; }
}

extern "C" int jb_encode(const unsigned char* pixels,int w,int h,void* context,Pixels copy) noexcept {
    try {
        std::vector<unsigned char> data;
        if (!cv::imencode(".png",cv::Mat(h,w,CV_8UC3,const_cast<unsigned char*>(pixels)),data) || data.size()>INT_MAX) return 0;
        copy(context, (int)data.size(), 1, data.data()); return 1;
    } catch (...) { return 0; }
}

namespace {
cv::Mat glyph_feature(const cv::Mat& glyph) {
    int width=std::clamp((int)std::nearbyint(glyph.cols*32.0/glyph.rows),1,24);
    cv::Mat fitted=cv::Mat::zeros(32,24,CV_32F), scaled;
    cv::resize(glyph,scaled,{width,32},0,0,cv::INTER_AREA);
    scaled.convertTo(fitted(cv::Rect((24-width)/2,0,width,32)),CV_32F,1.0/255);
    return fitted;
}

cv::Mat feature(const cv::Mat& glyph) {
    cv::Mat fitted = glyph_feature(glyph);
    fitted /= cv::norm(fitted)+1e-6;
    return fitted;
}
std::vector<cv::Mat> segment_glyphs(const cv::Mat& roi) {
    cv::Mat binary;
    cv::cvtColor(roi, binary, cv::COLOR_BGR2GRAY);
    cv::threshold(binary, binary, 0, 255, cv::THRESH_BINARY_INV | cv::THRESH_OTSU);
    cv::morphologyEx(binary, binary, cv::MORPH_CLOSE, cv::Mat::ones(3, 3, CV_8U));
    cv::Mat labels, stats, centers;
    const int count = cv::connectedComponentsWithStats(binary, labels, stats, centers, 8);
    std::vector<cv::Rect> boxes;
    const int min_height = std::max(6, static_cast<int>(.35 * roi.rows));
    for (int i = 1; i < count; ++i) {
        const int* s = stats.ptr<int>(i);
        if (s[3] >= min_height && s[4] >= 25) boxes.emplace_back(s[0], s[1], s[2], s[3]);
    }
    std::stable_sort(boxes.begin(), boxes.end(), [](cv::Rect a, cv::Rect b) { return a.x < b.x; });
    std::vector<cv::Mat> glyphs;
    for (auto box : boxes) glyphs.push_back(binary(box));
    return glyphs;
}
struct Templates { std::array<cv::Mat,12> items; };
cv::Mat gray(const unsigned char* data,int w,int h) {
    cv::Mat out; cv::cvtColor(cv::Mat(h,w,CV_8UC3,const_cast<unsigned char*>(data)),out,cv::COLOR_BGR2GRAY); return out;
}
}
// Raw grayscale exemplars are encoded losslessly. Rust compares the features,
// resolves score offsets and conflicting votes, and chooses which PNGs to save.
using GlyphSink = void(*)(void*, const float*, const unsigned char*, size_t);
extern "C" int jb_harvest_glyphs(const unsigned char* bgr, int width, int height,
                                void* context, GlyphSink emit) noexcept {
    try {
        const cv::Mat roi(height, width, CV_8UC3, const_cast<unsigned char*>(bgr));
        for (const cv::Mat& glyph : segment_glyphs(roi)) {
            const cv::Mat fitted = glyph_feature(glyph);
            std::vector<unsigned char> png;
            if (!cv::imencode(".png", glyph, png)) return 0;
            emit(context, fitted.ptr<float>(), png.data(), png.size());
        }
        return 1;
    } catch (...) { return 0; }
}

extern "C" void jb_cv_threads(int threads) {cv::setNumThreads(threads);}
extern "C" void* jb_templates() noexcept {try {return new Templates;} catch (...) {return nullptr;}}
extern "C" void jb_templates_drop(void* ctx) noexcept {delete static_cast<Templates*>(ctx);}
extern "C" int jb_template(void* ctx,int id,const unsigned char* data,int size) noexcept {
    try {
        auto image=cv::imdecode(cv::Mat(1,size,CV_8U,const_cast<unsigned char*>(data)),id<10?cv::IMREAD_GRAYSCALE:cv::IMREAD_COLOR);
        if(image.empty())return 0;
        auto& slot=static_cast<Templates*>(ctx)->items.at(id);
        if(id<10)slot=feature(image);else cv::cvtColor(image,slot,cv::COLOR_BGR2GRAY);
        return 1;
    }catch(...){return 0;}
}
using Similarities = void (*)(void*, const double*);
extern "C" int jb_digit_scores(void* ctx, const unsigned char* data, int w, int h,
                               const int* rect, void* output, Similarities emit) noexcept {
    try {
        cv::Mat image(h, w, CV_8UC3, const_cast<unsigned char*>(data));
        auto roi = image(cv::Rect(rect[0], rect[1], rect[2] - rect[0], rect[3] - rect[1]));
        const auto glyphs = segment_glyphs(roi);
        for (const auto& glyph : glyphs) {
            const auto f = feature(glyph);
            std::array<double, 10> scores;
            for (int i = 0; i != 10; ++i) scores[i] = f.dot(static_cast<Templates*>(ctx)->items[i]);
            emit(output, scores.data());
        }
        return !glyphs.empty();
    } catch (...) { return 0; }
}

extern "C" int jb_button_scores(void* ctx, const unsigned char* data, int w, int h,
                                double (*out)[3]) noexcept {
    try {
        const auto image = gray(data, w, h);
        for (int i = 0; i < 2; ++i) {
            const auto& tpl = static_cast<Templates*>(ctx)->items[10 + i];
            if (tpl.rows >= h || tpl.cols >= w) continue;
            cv::Mat scores;
            cv::matchTemplate(image, tpl, scores, cv::TM_CCOEFF_NORMED);
            cv::Point at;
            cv::minMaxLoc(scores, nullptr, &out[i][0], nullptr, &at);
            out[i][1] = at.x + tpl.cols * .5;
            out[i][2] = at.y + tpl.rows * .5;
        }
        return 1;
    } catch (...) { return 0; }
}
extern "C" int jb_motion(const unsigned char* data,int w,int h,int top,unsigned char* small) noexcept {
    try{
        cv::Mat image=gray(data,w,h);
        cv::Mat dst(176,96,CV_8U,small);
        cv::resize(image.rowRange(top,h),dst,dst.size(),0,0,cv::INTER_AREA);return 1;
    }catch(...){return 0;}
}
extern "C" int jb_flow(const unsigned char* before,const unsigned char* after,int w,int h,double* out) noexcept {
    try {
        cv::Mat a,b,window; gray(before,w,h).convertTo(a,CV_32F);gray(after,w,h).convertTo(b,CV_32F);
        cv::createHanningWindow(window,{w,h},CV_32F);
        auto shift=cv::phaseCorrelate(a,b,window,out+2);out[0]=shift.x;out[1]=shift.y;return 1;
    }catch(...){return 0;}
}
