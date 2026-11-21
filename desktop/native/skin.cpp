// Shape fallback for material-coloured pawns. All learned colour and texture
// samples belong to the current head in the current frame (skin_vision.py).
#include <opencv2/core.hpp>
#include <opencv2/imgproc.hpp>
#include <algorithm>
#include <array>
#include <cmath>
#include <utility>
#include <map>
#include <optional>
#include <vector>

namespace {
using cv::Mat;
using cv::Point;
using cv::Rect;
using Contour = std::vector<Point>;
using Contours = std::vector<Contour>;

struct Shape {
    Rect box;
    Contour contour;
    double solidity;
    bool repaired = false;
};

struct Head {
    Rect box;
    Contour hull;
    double circularity;
};

struct Platform {
    Rect box;
    double area;
    Contour hull;
};

// Shared with the Rust callbacks in skin.rs. Pixel data is borrowed for a
// single callback. All rectangles sent to Rust use full-frame coordinates.
struct Bounds { int x, y, w, h; };
Bounds bounds(Rect r, int top = 0) { return {r.x, r.y+top, r.width, r.height}; }

struct Candidate {
    size_t head_id;
    Bounds head, body, box;
    double roundness, solidity;
    int repaired, support_count;
    const unsigned char* mask;
    const int (*points)[2];
};

struct Rules {
    int (*head_kind)(int, Bounds, double, double);
    int (*upright)(const void*, size_t, Bounds, Bounds, double, int);
    int (*material_and_waist)(const int*, size_t, double, double, double, double);
    void (*consider)(void*, const Candidate*);
};

int rounded(double value) {
    return static_cast<int>(std::nearbyint(value)); // Python round ties to even
}

Mat filled(const Contour& contour, Rect box) {
    Mat mask = Mat::zeros(box.size(), CV_8U);
    cv::drawContours(mask, Contours{contour}, 0, 255, cv::FILLED, cv::LINE_8,
                     cv::noArray(), 0, -box.tl());
    return mask;
}

Mat morph(const Mat& plane, int operation, int width) {
    Mat result;
    cv::morphologyEx(plane, result, operation,
                     cv::getStructuringElement(cv::MORPH_RECT, {width, width}));
    return result;
}

Contours contours_of(const Mat& plane, int mode = cv::RETR_EXTERNAL, Point offset = {}) {
    Contours contours;
    cv::findContours(plane, contours, mode, cv::CHAIN_APPROX_SIMPLE, offset);
    return contours;
}

bool inside_border(Rect r, cv::Size size) {
    return r.x > 0 && r.y > 0 && r.br().x < size.width && r.br().y < size.height;
}

Mat inner_core(const Mat& mask) {
    Mat core;
    cv::erode(mask, core, Mat()); // 3x3 rectangle
    return core;
}

// The per-channel median is the average of the middle two order statistics,
// including half-integer colours in an even-sized mask.
double median_bin(const std::array<int, 256>& bins, int count) {
    int before = 0, low = -1;
    for (int i = 0; i < 256; ++i) {
        before += bins[i];
        if (low < 0 && before > (count - 1) / 2) low = i;
        if (before > count / 2) return (low + i) * 0.5;
    }
    return 0;
}

cv::Vec3d median_colour(const Mat& lab, const Mat& core) {
    std::array<std::array<int, 256>, 3> bins{};
    int count = 0;
    for (int y = 0; y < lab.rows; ++y) {
        const auto* pixels = lab.ptr<cv::Vec3b>(y);
        const auto* selected = core.ptr<uchar>(y);
        for (int x = 0; x < lab.cols; ++x) {
            if (!selected[x]) continue;
            ++count;
            for (int c = 0; c < 3; ++c) ++bins[c][pixels[x][c]];
        }
    }
    return {median_bin(bins[0], count), median_bin(bins[1], count), median_bin(bins[2], count)};
}

struct Material {
    Mat mask, core;
    cv::Vec3d colour;
    double chroma_tolerance;
};

std::optional<Material> sample_head(const Mat& lab, const Contour& head, Rect box) {
    Mat mask = filled(head, box), core = inner_core(mask);
    if (!cv::countNonZero(core)) return std::nullopt;
    const Mat patch = lab(box);
    const cv::Vec3d colour = median_colour(patch, core);
    std::vector<float> chroma;
    for (int y = 0; y < box.height; ++y) {
        const auto* pixels = patch.ptr<cv::Vec3b>(y);
        const auto* selected = core.ptr<uchar>(y);
        for (int x = 0; x < box.width; ++x) {
            if (selected[x]) {
                chroma.push_back(static_cast<float>(std::hypot(pixels[x][1] - colour[1],
                                                               pixels[x][2] - colour[2])));
            }
        }
    }
    std::sort(chroma.begin(), chroma.end());
    const double rank = (chroma.size() - 1) * .8;
    const size_t lower = static_cast<size_t>(rank);
    const double spread = chroma[lower] + (rank - lower) *
        (chroma[static_cast<size_t>(std::ceil(rank))] - chroma[lower]);
    return Material{mask, core, colour, std::clamp(spread + 6, 12., 20.)};
}

Mat material_plane(const Mat& lab, const Material& sample) {
    Mat plane(lab.size(), CV_8U);
    for (int y = 0; y < lab.rows; ++y) {
        const auto* row = lab.ptr<cv::Vec3b>(y);
        auto* out = plane.ptr<uchar>(y);
        for (int x = 0; x < lab.cols; ++x) {
            const cv::Vec3d delta = cv::Vec3d(row[x]) - sample.colour;
            const float chroma = std::sqrt(static_cast<float>(delta[1]*delta[1] + delta[2]*delta[2]));
            out[x] = delta.dot(delta) <= 35.*35 && chroma <= sample.chroma_tolerance ? 255 : 0;
        }
    }
    return plane;
}

Contours recover_heads(const Mat& lab, const Contour& fragment, Rect box) {
    auto sample = sample_head(lab, fragment, box);
    if (!sample) return {};
    const int pad = static_cast<int>(std::ceil(std::max(box.width, box.height) * .4));
    const Rect roi = Rect(box.x-pad, box.y-pad, box.width+2*pad, box.height+2*pad)
                     & Rect({}, lab.size());
    Mat plane = morph(material_plane(lab(roi), *sample), cv::MORPH_CLOSE, 3);
    Contours recovered;
    for (auto contour : contours_of(plane)) {
        if (!inside_border(cv::boundingRect(contour), plane.size())) continue;
        const Mat component = filled(contour, Rect({}, roi.size()));
        if (cv::countNonZero(component(box - roi.tl()) & sample->core) <
            .6 * cv::countNonZero(sample->core)) continue;
        for (Point& p : contour) p += roi.tl();
        recovered.push_back(std::move(contour));
    }
    return recovered;
}

// Close holes inside each original component's box so neighbouring cardboard
// seams cannot become part of the foot.
Contours repair_components(const Mat& plane, int width) {
    Mat labels, stats, centres;
    const int count = cv::connectedComponentsWithStats(plane, labels, stats, centres, 8);
    Contours contours;
    for (int i = 1; i < count; ++i) {
        const int* s = stats.ptr<int>(i);
        Rect box{s[0], s[1], s[2], s[3]};
        if (s[cv::CC_STAT_AREA] < 20 || !inside_border(box, plane.size())) continue;
        Mat component = labels(box) == i, padded;
        cv::copyMakeBorder(component, padded, width, width, width, width, cv::BORDER_CONSTANT, 0);
        const Mat closed = morph(padded, cv::MORPH_CLOSE, width);
        for (auto& contour : contours_of(closed(Rect({width, width}, box.size())),
                                         cv::RETR_EXTERNAL, box.tl()))
            contours.push_back(std::move(contour));
    }
    return contours;
}

// NumPy crop arguments to cv2 have independent borders even when their row
// stride comes from the full image. Use isolated borders for C++ Mat views.
Mat detail(const Mat& roi) {
    Mat blurred, delta;
    cv::GaussianBlur(roi, blurred, {3, 3}, 0, 0, cv::BORDER_DEFAULT | cv::BORDER_ISOLATED);
    cv::absdiff(roi, blurred, delta);
    if (roi.channels() == 1) return delta;
    std::vector<Mat> channels;
    cv::split(delta, channels);
    return cv::max(channels[0], cv::max(channels[1], channels[2]));
}

Mat substantial_texture(const Mat& region, int width) {
    return morph(morph(morph(region, cv::MORPH_CLOSE, 3), cv::MORPH_OPEN, width),
                 cv::MORPH_CLOSE, width);
}

Mat density_region(const Mat& material, const Mat& flecks, double coverage, int width) {
    Mat binary, density;
    flecks.convertTo(binary, CV_32F, 1./255);
    cv::blur(binary, density, {width, width});
    return substantial_texture(material & (density >= std::clamp(coverage * .6, .25, .5)), width);
}

void add_texture_planes(const Mat& lab, Rect head, Rect roi, const Material& sample,
                        const Mat& material, int width, std::vector<Mat>& planes) {
    Mat head_l;
    cv::extractChannel(lab(head), head_l, 0);
    const Mat texture = detail(head_l);
    std::array<int, 256> bins{};
    const int core_size = cv::countNonZero(sample.core);
    for (int y = 0; y < head.height; ++y)
        for (int x = 0; x < head.width; ++x)
            if (sample.core.at<uchar>(y, x)) ++bins[texture.at<uchar>(y, x)];
    const double grain = median_bin(bins, core_size);
    if (grain >= 1) {
        const double threshold = std::clamp(grain * .5, 2., 4.);
        Mat roi_l;
        cv::extractChannel(lab(roi), roi_l, 0);
        const Mat flecks = detail(roi_l) >= threshold;
        const double coverage = cv::countNonZero((texture >= threshold) & sample.core)
                              / static_cast<double>(core_size);
        planes.push_back(substantial_texture(material & flecks, width));
        planes.push_back(density_region(material, flecks, coverage, width));
    }
    const double fine_coverage = cv::countNonZero((detail(lab(head)) >= 1) & sample.core)
                               / static_cast<double>(core_size);
    if (fine_coverage >= .6)
        planes.push_back(density_region(material, detail(lab(roi)) >= 1, fine_coverage, width));
}

// Interpolation can erase head flecks and join the body to a flat platform.
// Repeated colours outside the body's search corridor describe that backdrop.
Mat without_backdrop(const Mat& patch, const Mat& material, int width) {
    std::map<std::array<uchar, 3>, int> counts;
    for (int y = 0; y < patch.rows; ++y) {
        const auto* row = patch.ptr<cv::Vec3b>(y);
        for (int x : {0, 1, patch.cols - 2, patch.cols - 1}) {
            const auto& p = row[x];
            ++counts[{p[0], p[1], p[2]}];
        }
    }
    Mat remaining = material.clone(), same;
    for (const auto& [p, count] : counts) {
        if (count < width) continue;
        const cv::Scalar colour(p[0], p[1], p[2]);
        const auto rounding = cv::Scalar::all(2);
        cv::inRange(patch, colour - rounding, colour + rounding, same);
        remaining.setTo(0, same);
    }
    return substantial_texture(remaining, width);
}

std::vector<Shape> material_bodies(const Mat& lab, Rect head, const Material& sample,
                                   bool separate_backdrop) {
    const double diameter = (head.width + head.height) * .5, cx = head.x + head.width * .5;
    const int left = std::max(0, static_cast<int>(std::floor(cx - diameter * .85)) - 2);
    const int right = std::min(lab.cols, static_cast<int>(std::ceil(cx + diameter * .85)) + 2);
    const int top = head.br().y;
    const int bottom = std::min(lab.rows, static_cast<int>(std::ceil(top + diameter * 4.15)) + 2);
    if (top >= bottom || left >= right) return {};
    const Rect roi(Point(left, top), Point(right, bottom));
    const Mat material = material_plane(lab(roi), sample);
    const int cap = std::max(3, rounded(lab.cols * 7. / 553) | 1);
    const int repair_width = std::clamp(rounded(diameter * .20) | 1, 3, cap);
    const int width = std::max(3, rounded(diameter * .14) | 1);
    std::vector<Mat> planes;
    size_t material_count = 0;
    if (separate_backdrop) {
        if (!cv::countNonZero(material.colRange(0, 2)) &&
            !cv::countNonZero(material.colRange(material.cols - 2, material.cols))) return {};
        planes.push_back(without_backdrop(lab(roi), material, width));
    } else {
        planes.push_back(morph(material, cv::MORPH_CLOSE, 3));
        if (repair_width > 3) planes.push_back(morph(material, cv::MORPH_CLOSE, repair_width));
        material_count = planes.size();
        add_texture_planes(lab, head, roi, sample, material, width, planes);
    }

    std::vector<Shape> bodies;
    auto add = [&](Contours contours, bool repaired) {
        for (auto& contour : contours) {
            const Rect box = cv::boundingRect(contour);
            if (!inside_border(box, roi.size())) continue;
            Contour hull;
            cv::convexHull(contour, hull);
            const double area = cv::contourArea(hull);
            if (area < 20) continue;
            const double solidity = cv::contourArea(contour) / area;
            for (Point& p : contour) p += roi.tl();
            bodies.push_back({box + roi.tl(), std::move(contour), solidity, repaired});
        }
    };
    for (const Mat& plane : planes) add(contours_of(plane), false);
    if (repair_width > 3)
        for (size_t i = material_count; i < planes.size(); ++i)
            add(repair_components(planes[i], repair_width), true);
    return bodies;
}

// A nearly white top may have no block-mask pixels. Only one broad, closed
// platform hull witnessing two separate rows under the foot can replace it.
bool supported(const Mat& mask, int x, int y, int top, double diameter,
               const std::vector<Platform>& platforms, std::vector<Point>& best) {
    if (mask.empty()) return true;
    const int left = std::max(0, x-4), right = std::min(mask.cols, x+5);
    constexpr int deltas[] = {6, 10, 14, 18, 24};
    int hits = 0;
    for (int delta : deltas) {
        if (y+delta < mask.rows && cv::countNonZero(mask.row(y+delta).colRange(left, right)))
            ++hits;
    }
    if (hits >= 2) return true;
    for (const Platform& p : platforms) {
        if (p.box.width < diameter*2 || p.box.height < diameter || p.area < diameter*diameter*2)
            continue;
        std::vector<Point> witness;
        int rows = 0;
        for (int delta : deltas) {
            int sy = y+delta;
            if (sy >= mask.rows) continue;
            const size_t start = witness.size();
            for (int px = left; px < right; ++px)
                if (cv::pointPolygonTest(p.hull, cv::Point2f(float(px), float(sy-top)), false) >= 0)
                    witness.emplace_back(px, sy);
            if (witness.size()-start >= 5) ++rows;
            else witness.resize(start);
        }
        if (rows >= 2 && witness.size() > best.size()) best = std::move(witness);
    }
    return !best.empty();
}

bool measure_body(const Mat& lab, const Mat& support, int top, size_t head_id,
                  const Head& head, const Material& sample, const Shape& body,
                  const std::vector<Platform>& platforms, void* context, const Rules& rules) {
    const Rect b = body.box, h = head.box;
    const Bounds head_box = bounds(h, top), body_box = bounds(b, top);
    if (!rules.upright(context, head_id, head_box, body_box, body.solidity, body.repaired)) return false;
    const Mat mask = filled(body.contour, b);
    const Mat core = inner_core(mask);
    if (!cv::countNonZero(core)) return false;
    const Mat patch = lab(b);
    const double diameter = (h.width + h.height) * .5;
    const int start = std::max(0, b.height - std::max(3, rounded(diameter * .25)));
    const Mat base = core.rowRange(start, b.height);
    if (!cv::countNonZero(base)) return false;
    const cv::Vec3d base_delta = sample.colour - median_colour(patch.rowRange(start, b.height), base);
    std::vector<int> widths(b.height);
    for (int y = 0; y < b.height; ++y) widths[y] = cv::countNonZero(mask.row(y));
    if (!rules.material_and_waist(widths.data(), widths.size(),
            cv::norm(sample.colour - median_colour(patch, core)), cv::norm(base_delta),
            std::hypot(base_delta[1], base_delta[2]), sample.chroma_tolerance)) return false;

    std::vector<Point> support_pixels;
    if (!supported(support, rounded(b.x + b.width*.5), b.br().y + top,
                   top, diameter, platforms, support_pixels)) return false;
    // Each support witness contains at most five rows of nine pixels.
    int points[45][2];
    for (size_t i = 0; i < support_pixels.size(); ++i) {
        points[i][0] = support_pixels[i].x;
        points[i][1] = support_pixels[i].y;
    }
    const Rect box = h | b;
    Mat silhouette = Mat::zeros(box.size(), CV_8U);
    silhouette(h - box.tl()) |= sample.mask;
    silhouette(b - box.tl()) |= mask;
    Candidate candidate{head_id, head_box, body_box, bounds(box, top),
                        head.circularity, body.solidity, body.repaired,
                        static_cast<int>(support_pixels.size()), silhouette.data, points};
    rules.consider(context, &candidate);
    return true;
}

double circularity(const Contour& hull, double area) {
    const double perimeter = cv::arcLength(hull, true);
    return 4 * CV_PI * area / std::max(perimeter*perimeter, 1.);
}

void visit_skin(const Mat& frame, const Mat& support, bool allow_backdrop, void* context, const Rules& rules) {
    const int height = frame.rows, width = frame.cols, top = static_cast<int>(height * .35);
    if (!support.empty()) {
        const int active = cv::countNonZero(support);
        if (!active || double(active) / frame.total() > .90) return;
    }
    const Mat crop = frame.rowRange(top, height);
    Mat lab, gray, edges;
    cv::cvtColor(crop, lab, cv::COLOR_BGR2Lab);
    cv::cvtColor(crop, gray, cv::COLOR_BGR2GRAY);
    cv::Canny(gray, edges, 25, 70);
    std::vector<Mat> channels;
    cv::split(lab, channels);
    for (const Mat& channel : channels) {
        Mat edge;
        cv::Canny(channel, edge, 20, 60);
        edges |= edge;
    }

    std::vector<Head> heads;
    std::vector<Platform> platforms;
    std::vector<Shape> shapes;
    for (auto& contour : contours_of(edges, cv::RETR_LIST)) {
        const Rect b = cv::boundingRect(contour);
        if (b.x <= 1 || b.br().x >= width-1 || b.width < 5 || b.height < 5) continue;
        Contour hull;
        cv::convexHull(contour, hull);
        const double area = cv::contourArea(hull);
        if (area < 20) continue;
        const double solidity = cv::contourArea(contour) / area;
        shapes.push_back({b, std::move(contour), solidity});
        if (b.width >= width*.08 && area >= frame.total()*.003)
            platforms.push_back({b, area, hull});
        const double roundness = circularity(hull, area);
        const int kind = rules.head_kind(width, bounds(b, top), roundness, solidity);
        if (kind == 1) {
            heads.push_back({b, std::move(hull), roundness});
        } else if (kind == 2) {
            for (const Contour& recovered : recover_heads(lab, hull, b)) {
                Contour rh;
                cv::convexHull(recovered, rh);
                const double ra = cv::contourArea(rh);
                if (ra < 20) continue;
                const Rect rb = cv::boundingRect(recovered);
                const double rc = circularity(rh, ra);
                if (rules.head_kind(width, bounds(rb, top), rc, cv::contourArea(recovered) / ra) == 1)
                    heads.push_back({rb, std::move(rh), rc});
            }
        }
    }

    auto visit_heads = [&](bool separate_backdrop) {
        bool matched = false;
        for (size_t i = 0; i < heads.size(); ++i) {
            const Head& head = heads[i];
            const auto sample = sample_head(lab, head.hull, head.box);
            if (!sample) continue;
            auto measure = [&](const Shape& body) {
                if (measure_body(lab, support, top, i, head, *sample, body, platforms, context, rules))
                    matched = true;
            };
            if (!separate_backdrop)
                for (const Shape& body : shapes) measure(body);
            for (const Shape& body : material_bodies(lab, head.box, *sample, separate_backdrop))
                measure(body);
        }
        return matched;
    };
    // An ambiguous first pass already has candidates. Never retry to pick a winner.
    if (!visit_heads(false) && allow_backdrop) visit_heads(true);
}
} // namespace

extern "C" int jb_skin_visit(const unsigned char* bgr, int width, int height,
                             const unsigned char* support, int allow_backdrop, void* context, const Rules* rules) noexcept {
    try {
        const Mat frame(height, width, CV_8UC3, const_cast<unsigned char*>(bgr));
        Mat platform;
        if (support) platform = Mat(height, width, CV_8U, const_cast<unsigned char*>(support));
        visit_skin(frame, platform, allow_backdrop != 0, context, *rules);
        return 1;
    } catch (...) {
        return 0;
    }
}
