// bench-opencv: the OpenCV (cv::dnn / FaceDetectorYN / FaceRecognizerSF)
// reference measured like-for-like with nirlock-bench: same lit frames,
// same crops, same thread count, same loop shape, same statistics
// (fuprobe `summarise`: mean-of-middle median, nearest-rank p90).
//
// Differences that cannot be removed and are stated in BENCH.md:
//  * fuprobe's factories run one warm-up forward inside load, so "load"
//    here INCLUDES the first inference and "first" is measured after it;
//  * OpenCV has one global thread pool (cv::setNumThreads) for all models,
//    ORT has one pool per session (YuNet 1 thread, embedders N).
//
// --dump FILE writes per frame: file, index, box, landmarks, both embeddings
// (biometric data: keep it in the lab data directory or in tmpfs).
#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdio>
#include <cstring>
#include <filesystem>
#include <fstream>
#include <opencv2/core.hpp>
#include <opencv2/core/utils/logger.hpp>
#include <opencv2/imgcodecs.hpp>
#include <string>
#include <thread>
#include <vector>

#include "pipeline.hpp"

namespace fs = std::filesystem;
using clk = std::chrono::steady_clock;

static double ms_since(clk::time_point t) {
    return std::chrono::duration<double, std::milli>(clk::now() - t).count();
}

struct Summary {
    int n = 0;
    double median = NAN, p90 = NAN, min = NAN, max = NAN;
};
static Summary summarise(std::vector<double> v) {
    Summary s;
    s.n = int(v.size());
    if (v.empty()) return s;
    std::sort(v.begin(), v.end());
    size_t n = v.size();
    s.median = n % 2 ? v[n / 2] : 0.5 * (v[n / 2 - 1] + v[n / 2]);
    s.p90 = v[std::min(n - 1, size_t(std::ceil(0.9 * double(n))) - 1)];
    s.min = v.front();
    s.max = v.back();
    return s;
}

static long status_kib(const char* key) {
    std::ifstream f("/proc/self/status");
    std::string line;
    while (std::getline(f, line))
        if (line.rfind(key, 0) == 0) return std::atol(line.c_str() + strlen(key));
    return -1;
}

static std::string read_trim(const char* path) {
    std::ifstream f(path);
    std::string s;
    std::getline(f, s);
    return s.empty() ? "unknown" : s;
}

static std::string power_source() {
    for (auto& e : fs::directory_iterator("/sys/class/power_supply")) {
        std::string name = e.path().filename();
        if (name.rfind("AC", 0) != 0) continue;
        std::ifstream on(e.path() / "online");
        int v = -1;
        on >> v;
        if (v == 1) return "AC (" + name + " online)";
        if (v == 0) return "battery (" + name + " offline)";
    }
    return "unknown";
}

// Lit frames per frames.jsonl (meta_lit=true), or all ir_*.pgm without it.
static std::vector<std::pair<std::string, int>> lit_frames(const fs::path& dir) {
    std::vector<std::pair<std::string, int>> out;
    std::ifstream jl(dir / "frames.jsonl");
    if (jl) {
        std::string line;
        while (std::getline(jl, line)) {
            if (line.find("\"meta_lit\":true") == std::string::npos) continue;
            auto f = line.find("\"file\":\"");
            if (f == std::string::npos) continue;
            f += 8;
            auto e = line.find('"', f);
            std::string file = line.substr(f, e - f);
            int idx = -1;
            auto i = line.find("\"index\":");
            if (i != std::string::npos) idx = std::atoi(line.c_str() + i + 8);
            out.emplace_back(file, idx);
        }
    } else {
        for (auto& e : fs::directory_iterator(dir)) {
            std::string n = e.path().filename();
            if (n.rfind("ir_", 0) == 0 && n.size() > 4 && n.substr(n.size() - 4) == ".pgm")
                out.emplace_back(n, std::atoi(n.c_str() + 3));
        }
    }
    std::sort(out.begin(), out.end());
    return out;
}

template <class F>
static std::vector<double> steady(int iters, double pace_ms, F&& f) {
    std::vector<double> t;
    t.reserve(iters);
    for (int i = 0; i < iters; i++) {
        auto t0 = clk::now();
        f(i);
        t.push_back(ms_since(t0));
        if (pace_ms > 0) {
            double left = pace_ms - ms_since(t0);
            if (left > 0) std::this_thread::sleep_for(std::chrono::duration<double, std::milli>(left));
        }
    }
    return t;
}

static void print_row(const char* name, double load, double first, const Summary& s, long rss_kib, long delta_kib) {
    printf("%-9s load %7.1f ms | first %7.1f ms | steady n=%d median %7.2f ms p90 %7.2f ms min %7.2f max %7.2f | RSS after load %ld MiB (%+ld MiB)\n",
           name, load, first, s.n, s.median, s.p90, s.min, s.max, rss_kib / 1024, delta_kib / 1024);
}

int main(int argc, char** argv) {
    std::string models, frames, dump;
    int threads = 4, iters = 40;
    double pace_ms = 0;
    for (int i = 1; i < argc; i++) {
        std::string a = argv[i];
        auto val = [&]() -> std::string { return i + 1 < argc ? argv[++i] : ""; };
        if (a == "--models") models = val();
        else if (a == "--frames") frames = val();
        else if (a == "--threads") threads = std::atoi(val().c_str());
        else if (a == "--iters") iters = std::atoi(val().c_str());
        else if (a == "--pace-ms") pace_ms = std::atof(val().c_str());
        else if (a == "--dump") dump = val();
        else {
            fprintf(stderr, "usage: bench-opencv --models DIR --frames DIR [--threads N] [--iters N] [--pace-ms N] [--dump FILE.jsonl]\n");
            return 2;
        }
    }
    if (models.empty() || frames.empty()) {
        fprintf(stderr, "bench-opencv: --models and --frames are required\n");
        return 2;
    }
    if (iters < 30) iters = 30;
    cv::utils::logging::setLogLevel(cv::utils::logging::LOG_LEVEL_ERROR);
    cv::setNumThreads(threads);

    printf("opencv    : %s (cv::dnn, cv::setNumThreads(%d) global pool)\n", CV_VERSION, threads);
    printf("power     : %s | governor %s | epp %s | platform_profile %s\n", power_source().c_str(),
           read_trim("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor").c_str(),
           read_trim("/sys/devices/system/cpu/cpu0/cpufreq/energy_performance_preference").c_str(),
           read_trim("/sys/firmware/acpi/platform_profile").c_str());

    fs::path dir(frames);
    auto list = lit_frames(dir);
    std::vector<cv::Mat> images;
    for (auto& [file, idx] : list) {
        cv::Mat m = cv::imread((dir / file).string(), cv::IMREAD_GRAYSCALE);
        if (m.empty()) {
            fprintf(stderr, "cannot read %s\n", file.c_str());
            return 1;
        }
        images.push_back(m);
    }
    if (images.empty()) {
        fprintf(stderr, "no frames\n");
        return 1;
    }
    printf("frames    : %zu lit frames in %s, %dx%d; iters %d; pace %.0f ms\n", images.size(), frames.c_str(),
           images[0].cols, images[0].rows, iters, pace_ms);

    // Load everything first, daemon order: detector, AuraFace, SFace.
    // (fuprobe's factories include one warm-up forward each.)
    long rss0 = status_kib("VmRSS:");
    auto t0 = clk::now();
    fu::Detector det(models + "/" + fu::kYunetFile);
    double load_y = ms_since(t0);
    long rss_y = status_kib("VmRSS:");
    t0 = clk::now();
    auto aura = fu::make_auraface(models + "/" + fu::kAurafaceFile);
    double load_a = ms_since(t0);
    long rss_a = status_kib("VmRSS:");
    t0 = clk::now();
    auto sface = fu::make_sface(models + "/" + fu::kSfaceFile);
    double load_s = ms_since(t0);
    long rss_s = status_kib("VmRSS:");
    long hwm_loads = status_kib("VmHWM:");
    printf("loaded    : yunet %.0f ms, auraface %.0f ms, sface %.0f ms (each includes one warm-up forward) | RSS %ld → %ld → %ld → %ld MiB, VmHWM %ld MiB\n\n",
           load_y, load_a, load_s, rss0 / 1024, rss_y / 1024, rss_a / 1024, rss_s / 1024, hwm_loads / 1024);

    // First (post-warm-up) inference, then steady loops.
    t0 = clk::now();
    auto first_faces = det.detect(images[0]);
    double first_y = ms_since(t0);
    auto ys = summarise(steady(iters, pace_ms, [&](int i) { det.detect(images[i % images.size()]); }));
    print_row("yunet", load_y, first_y, ys, rss_y, rss_y - rss0);

    // Detect every frame once; keep best face and aligned crop.
    struct Per { bool has = false; fu::Face face; cv::Mat crop; };
    std::vector<Per> per(images.size());
    std::vector<cv::Mat> crops;
    for (size_t i = 0; i < images.size(); i++) {
        auto faces = det.detect(images[i]);
        for (auto& f : faces)
            if (f.score >= fu::kFaceReportScore) {
                per[i].has = true;
                per[i].face = f;
                per[i].crop = fu::align_face(images[i], f);
                crops.push_back(per[i].crop);
                break;
            }
    }
    if (crops.empty()) {
        fprintf(stderr, "no face in any frame\n");
        return 1;
    }
    fu::Embedder* embs[2] = {aura.get(), sface.get()};
    double loads[2] = {load_a, load_s};
    long rss[2] = {rss_a, rss_s}, before[2] = {rss_y, rss_a};
    std::vector<std::vector<cv::Mat>> embeddings(2, std::vector<cv::Mat>(images.size()));
    for (int k = 0; k < 2; k++) {
        t0 = clk::now();
        embs[k]->embed(crops[0]);
        double first = ms_since(t0);
        auto s = summarise(steady(iters, pace_ms, [&](int i) { embs[k]->embed(crops[i % crops.size()]); }));
        print_row(embs[k]->name().c_str(), loads[k], first, s, rss[k], rss[k] - before[k]);
        for (size_t i = 0; i < images.size(); i++)
            if (per[i].has) embeddings[k][i] = embs[k]->embed(per[i].crop);
    }
    printf("\nmemory    : RSS after loads %ld MiB (VmHWM %ld), at end %ld MiB (VmHWM %ld)\n", rss_s / 1024,
           hwm_loads / 1024, status_kib("VmRSS:") / 1024, status_kib("VmHWM:") / 1024);

    if (!dump.empty()) {
        std::ofstream out(dump);
        for (size_t i = 0; i < images.size(); i++) {
            out << "{\"file\":\"" << list[i].first << "\",\"index\":" << list[i].second;
            if (per[i].has) {
                auto& f = per[i].face;
                out << ",\"face\":{\"x\":" << f.box.x << ",\"y\":" << f.box.y << ",\"w\":" << f.box.width
                    << ",\"h\":" << f.box.height << ",\"score\":" << f.score << ",\"lm\":[";
                for (int j = 0; j < 5; j++) out << (j ? "," : "") << "[" << f.lm[j].x << "," << f.lm[j].y << "]";
                out << "]}";
                const char* names[2] = {"auraface", "sface"};
                for (int k = 0; k < 2; k++) {
                    out << ",\"" << names[k] << "\":[";
                    const cv::Mat& e = embeddings[k][i];
                    for (int j = 0; j < e.cols; j++) out << (j ? "," : "") << e.at<float>(0, j);
                    out << "]";
                }
            } else {
                out << ",\"face\":null,\"auraface\":null,\"sface\":null";
            }
            out << "}\n";
        }
        printf("dump      : %s\n", dump.c_str());
    }
    return 0;
}
