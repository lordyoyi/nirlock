# bench-opencv: like-for-like OpenCV cv::dnn reference for docs/BENCH.md.
# Links the lab's phase0/pipeline.cpp (the fuprobe pipeline) unchanged.
#
#   make LAB=/path/to/zenbook-face-unlock
#   build/bench-opencv --models DIR --frames DIR --threads N --iters N [--dump FILE.jsonl] [--pace-ms N]
LAB   ?= $(HOME)/Work/zenbook-face-unlock
CXX   ?= g++
OPT   ?= -O2
BUILD := build
override CXXFLAGS := $(OPT) $(CXXFLAGS) -std=c++20 -Wall -Wextra -pthread -I$(LAB)/phase0 $(shell pkg-config --cflags opencv5)
LDLIBS := $(filter -L%,$(shell pkg-config --libs opencv5)) \
          -lopencv_objdetect -lopencv_dnn -lopencv_imgcodecs -lopencv_imgproc -lopencv_core -pthread

all: $(BUILD)/bench-opencv

$(BUILD)/bench-opencv: $(BUILD)/bench_opencv.o $(BUILD)/pipeline.o
	$(CXX) $(CXXFLAGS) -o $@ $^ $(LDLIBS)

$(BUILD)/bench_opencv.o: bench_opencv.cpp $(LAB)/phase0/pipeline.hpp Makefile | $(BUILD)
	$(CXX) $(CXXFLAGS) -c -o $@ $<

$(BUILD)/pipeline.o: $(LAB)/phase0/pipeline.cpp $(LAB)/phase0/pipeline.hpp Makefile | $(BUILD)
	$(CXX) $(CXXFLAGS) -c -o $@ $<

$(BUILD):
	mkdir -p $(BUILD)

clean:
	rm -rf $(BUILD)

.PHONY: all clean
