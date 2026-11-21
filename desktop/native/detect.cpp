#include <opencv2/core.hpp>
#include <opencv2/imgproc.hpp>

#include <cmath>
#include <cstdint>
#include <memory>
#include <vector>

namespace {
struct Components {
    cv::Mat labels, stats;
};

// The output borrows both arrays until jb_regions_free. OpenCV orders stats
// as [left, top, width, height, area]; row zero describes the background.
struct Regions {
    const int* labels;
    const int* stats;
    int count;
    void* owner;
};

int label(const cv::Mat& mask, Regions* out) {
    auto parts = std::make_unique<Components>();
    cv::Mat centroids;
    int count = cv::connectedComponentsWithStats(mask, parts->labels, parts->stats,
                                                centroids, 8, CV_32S);
    *out = {parts->labels.ptr<int>(), parts->stats.ptr<int>(), count, parts.get()};
    parts.release();
    return 0;
}

cv::Mat mono(const uint8_t* pixels, int w, int h) {
    return cv::Mat(h, w, CV_8UC1, const_cast<uint8_t*>(pixels));
}

cv::Mat bgr(const uint8_t* pixels, int w, int h) {
    return cv::Mat(h, w, CV_8UC3, const_cast<uint8_t*>(pixels));
}

cv::Mat square(int size) {
    return cv::getStructuringElement(cv::MORPH_RECT, cv::Size(size, size));
}
}

extern "C" {
void jb_regions_free(void* owner) noexcept {
    delete static_cast<Components*>(owner);
}

int jb_piece_regions(const uint8_t* pixels, int w, int h, Regions* out) noexcept {
    try {
        cv::Mat clean;
        auto kernel = square(5);
        cv::morphologyEx(mono(pixels, w, h), clean, cv::MORPH_OPEN, kernel);
        cv::morphologyEx(clean, clean, cv::MORPH_CLOSE, kernel);
        return label(clean, out);
    } catch (...) { return -1; }
}

int jb_block_regions(const uint8_t* pixels, const uint8_t* piece, int w, int h,
                     Regions* out) noexcept {
    try {
        cv::Mat clean = mono(pixels, w, h).clone(), grown;
        cv::dilate(mono(piece, w, h), grown, square(9), cv::Point(-1, -1), 2);
        clean.setTo(0, grown);
        cv::morphologyEx(clean, clean, cv::MORPH_CLOSE, square(5), cv::Point(-1, -1), 2);
        cv::morphologyEx(clean, clean, cv::MORPH_OPEN, square(3));
        std::vector<std::vector<cv::Point>> contours;
        cv::findContours(clean, contours, cv::RETR_EXTERNAL, cv::CHAIN_APPROX_SIMPLE);
        clean.setTo(0);
        cv::drawContours(clean, contours, -1, 255, cv::FILLED);
        return label(clean, out);
    } catch (...) { return -1; }
}

int jb_dot_regions(const uint8_t* pixels, int w, int h, const int* bounds,
                   int min_value, int max_sat, Regions* out) noexcept {
    try {
        const cv::Rect roi(bounds[0], bounds[1], bounds[2], bounds[3]);
        cv::Mat hsv, mask;
        cv::cvtColor(bgr(pixels, w, h)(roi), hsv, cv::COLOR_BGR2HSV);
        cv::inRange(hsv, cv::Scalar(0, 0, min_value), cv::Scalar(180, max_sat, 255), mask);
        return label(mask, out);
    } catch (...) { return -1; }
}

int jb_piece_score(const uint8_t* pixels, int w, int h, const int* bounds,
                   const uint8_t* tpl, int tw, int th, double* score) noexcept {
    try {
        const cv::Rect roi(bounds[0], bounds[1], bounds[2], bounds[3]);
        cv::Mat crop, a, b;
        cv::resize(bgr(pixels, w, h)(roi), crop, cv::Size(tw, th));
        // The Python bbox score centers all interleaved channels together.
        // matchTemplate below centers each channel separately.
        crop.reshape(1).convertTo(a, CV_32F);
        bgr(tpl, tw, th).reshape(1).convertTo(b, CV_32F);
        a -= cv::mean(a)[0];
        b -= cv::mean(b)[0];
        *score = a.dot(b) / (cv::norm(a) * cv::norm(b) + 1e-6);
        return 0;
    } catch (...) { return -1; }
}

int jb_piece_scan(const uint8_t* pixels, int w, int h, int top,
                  const uint8_t* tpl, int tw, int th,
                  int* x, int* y, double* score) noexcept {
    try {
        cv::Mat matches;
        auto search = bgr(pixels, w, h).rowRange(top, h);
        cv::matchTemplate(search, bgr(tpl, tw, th), matches, cv::TM_CCOEFF_NORMED);
        cv::Point point;
        cv::minMaxLoc(matches, nullptr, score, nullptr, &point);
        *x = point.x;
        *y = point.y + top;
        return 0;
    } catch (...) { return -1; }
}
}
