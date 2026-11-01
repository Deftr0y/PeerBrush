// Copyright (c) the JPEG XL Project Authors. All rights reserved.
// Use of this source code is governed by the BSD-style vendor libjxl/LICENSE.
// Standalone scalar differential test; no image decoder, app or large fixture.
#include "lib/jxl/enc_sharper_rows.h"
#include <cassert>
#include <cstring>
#include <iostream>
#include <utility>
#include <vector>
#ifdef NDEBUG
#error "This standalone differential test requires active assertions"
#endif

// Independent copy of the pinned scalar implementation's box/mask/kernel
// arithmetic, retained as the old behavior oracle rather than calling Region.
static std::vector<float> Original(const std::vector<float> &input, size_t sw,
                                   size_t sh) {
  static const float kernel[144] = {
      -0.000314256996835, -0.000314256996835, -0.000897597057705,
      -0.000562751488849, -0.000176807273646, 0.001864627368902,
      0.001864627368902,  -0.000176807273646, -0.000562751488849,
      -0.000897597057705, -0.000314256996835, -0.000314256996835,
      -0.000314256996835, -0.001527942804748, -0.000121760530512,
      0.000191123989093,  0.010193185932466,  0.058637519197110,
      0.058637519197110,  0.010193185932466,  0.000191123989093,
      -0.000121760530512, -0.001527942804748, -0.000314256996835,
      -0.000897597057705, -0.000121760530512, 0.000946363683751,
      0.007113577630288,  0.000437956841058,  -0.000372823835211,
      -0.000372823835211, 0.000437956841058,  0.007113577630288,
      0.000946363683751,  -0.000121760530512, -0.000897597057705,
      -0.000562751488849, 0.000191123989093,  0.007113577630288,
      0.044592622228814,  0.000222278879007,  -0.162864473015945,
      -0.162864473015945, 0.000222278879007,  0.044592622228814,
      0.007113577630288,  0.000191123989093,  -0.000562751488849,
      -0.000176807273646, 0.010193185932466,  0.000437956841058,
      0.000222278879007,  -0.000913092543974, -0.017071696107902,
      -0.017071696107902, -0.000913092543974, 0.000222278879007,
      0.000437956841058,  0.010193185932466,  -0.000176807273646,
      0.001864627368902,  0.058637519197110,  -0.000372823835211,
      -0.162864473015945, -0.017071696107902, 0.414660099370354,
      0.414660099370354,  -0.017071696107902, -0.162864473015945,
      -0.000372823835211, 0.058637519197110,  0.001864627368902,
      0.001864627368902,  0.058637519197110,  -0.000372823835211,
      -0.162864473015945, -0.017071696107902, 0.414660099370354,
      0.414660099370354,  -0.017071696107902, -0.162864473015945,
      -0.000372823835211, 0.058637519197110,  0.001864627368902,
      -0.000176807273646, 0.010193185932466,  0.000437956841058,
      0.000222278879007,  -0.000913092543974, -0.017071696107902,
      -0.017071696107902, -0.000913092543974, 0.000222278879007,
      0.000437956841058,  0.010193185932466,  -0.000176807273646,
      -0.000562751488849, 0.000191123989093,  0.007113577630288,
      0.044592622228814,  0.000222278879007,  -0.162864473015945,
      -0.162864473015945, 0.000222278879007,  0.044592622228814,
      0.007113577630288,  0.000191123989093,  -0.000562751488849,
      -0.000897597057705, -0.000121760530512, 0.000946363683751,
      0.007113577630288,  0.000437956841058,  -0.000372823835211,
      -0.000372823835211, 0.000437956841058,  0.007113577630288,
      0.000946363683751,  -0.000121760530512, -0.000897597057705,
      -0.000314256996835, -0.001527942804748, -0.000121760530512,
      0.000191123989093,  0.010193185932466,  0.058637519197110,
      0.058637519197110,  0.010193185932466,  0.000191123989093,
      -0.000121760530512, -0.001527942804748, -0.000314256996835,
      -0.000314256996835, -0.000314256996835, -0.000897597057705,
      -0.000562751488849, -0.000176807273646, 0.001864627368902,
      0.001864627368902,  -0.000176807273646, -0.000562751488849,
      -0.000897597057705, -0.000314256996835, -0.000314256996835};
  const size_t w = (sw + 1) / 2, h = (sh + 1) / 2;
  std::vector<float> box(w * h), mask(w * h), output(w * h);
  for (size_t y = 0; y < h; ++y)
    for (size_t x = 0; x < w; ++x) {
      float sum = 0;
      size_t count = 0;
      for (size_t iy = 0; iy < 2 && iy + 2 * y < sh; ++iy)
        for (size_t ix = 0; ix < 2 && ix + 2 * x < sw; ++ix) {
          sum += input[(2 * y + iy) * sw + 2 * x + ix];
          ++count;
        }
      box[y * w + x] = sum / count;
    }
  for (size_t y = 0; y < h; ++y)
    for (size_t x = 0; x < w; ++x) {
      const float c = box[y * w + x], west = box[y * w + (x > 0 ? x - 1 : x)],
                  east = box[y * w + (x + 1 < w ? x + 1 : x)],
                  north = box[(y > 0 ? y - 1 : y) * w + x],
                  south = box[(y + 1 < h ? y + 1 : y) * w + x];
      const float values[4] = {std::abs(c - west), std::abs(c - east),
                               std::abs(c - north), std::abs(c - south)};
      float minimum = std::numeric_limits<float>::max(),
            second = std::numeric_limits<float>::max();
      for (float v : values) {
        if (v < second) {
          if (v < minimum) {
            second = minimum;
            minimum = v;
          } else {
            second = v;
          }
        }
      }
      mask[y * w + x] = second;
    }
  for (size_t y = 0; y < h; ++y) {
    const float *row_in[12];
    for (int64_t ky = 0; ky < 12; ++ky) {
      int64_t iy = y * 2 + ky - 5;
      if (iy < 0)
        iy = 0;
      if (iy >= static_cast<int64_t>(sh))
        iy = sh - 1;
      row_in[ky] = input.data() + iy * sw;
    }
    for (size_t x = 0; x < w; ++x) {
      float minimum = std::numeric_limits<float>::max(),
            maximum = std::numeric_limits<float>::min();
      for (int64_t ky = 5; ky + 5 < 12; ++ky)
        for (int64_t kx = 5; kx + 5 < 12; ++kx) {
          int64_t ix = x * 2 + kx - 5;
          if (ix < 0)
            ix = 0;
          if (ix >= static_cast<int64_t>(sw))
            ix = sw - 1;
          minimum = std::min<float>(minimum, row_in[ky][ix]);
          maximum = std::max<float>(maximum, row_in[ky][ix]);
        }
      float sum = 0;
      for (int64_t ky = 0; ky < 12; ++ky)
        for (int64_t kx = 0; kx < 12; ++kx) {
          int64_t ix = x * 2 + kx - 5;
          if (ix < 0)
            ix = 0;
          if (ix >= static_cast<int64_t>(sw))
            ix = sw - 1;
          sum += row_in[ky][ix] * kernel[ky * 12 + kx];
        }
      output[y * w + x] = sum;
      const float a = mask[y * w + x] * 1;
      const float low = minimum - a, high = maximum + a;
      if (output[y * w + x] < low)
        output[y * w + x] = low;
      else if (output[y * w + x] > high)
        output[y * w + x] = high;
    }
  }
  return output;
}
static void Case(size_t sw, size_t sh) {
  assert(sw * sh * sizeof(float) <= 1024 * 1024);
  std::vector<float> input(sw * sh);
  for (size_t y = 0; y < sh; ++y)
    for (size_t x = 0; x < sw; ++x) {
      float v =
          (static_cast<int>((x * 73 + y * 137) % 10000) - 5000) / 10000.0f;
      if ((x + y) % 17 == 0)
        v = 0.0f;
      if ((x + y) % 31 == 0)
        v = -0.0f;
      input[y * sw + x] = v;
    }
  const size_t w = (sw + 1) / 2, h = (sh + 1) / 2;
  const auto original = Original(input, sw, sh);
  std::vector<float> output(w * h, 211.0f),
      workspace((jxl::sharper_rows::kRows + 1) *
                std::min(jxl::sharper_rows::kStride, sw));
  size_t reads = 0, writes = 0;
  // Uneven regional boundaries cover true image edges and interior halos.
  for (size_t x = 0; x < w; x += 127) {
    for (size_t y = 0; y < h; y += 7) {
      assert(jxl::sharper_rows::Region(
          sw, sh, x, y, std::min<size_t>(127, w - x),
          std::min<size_t>(7, h - y), workspace.data(), workspace.size(),
          [&](size_t sy, size_t sx, size_t count, float *target) {
            assert(sy < sh && sx + count <= sw);
            ++reads;
            std::copy(input.data() + sy * sw + sx,
                      input.data() + sy * sw + sx + count, target);
            return true;
          },
          [&](size_t dy, size_t dx, size_t count, const float *source) {
            assert(dy < h && dx + count <= w);
            ++writes;
            std::copy(source, source + count, output.data() + dy * w + dx);
            return true;
          }));
    }
  }
  assert(writes > 0 && reads > 0);
  assert(std::memcmp(original.data(), output.data(),
                     original.size() * sizeof(float)) == 0);
}
int main() {
  for (auto shape : std::vector<std::pair<size_t, size_t>>{{1, 1},
                                                           {1, 17},
                                                           {17, 1},
                                                           {17, 19},
                                                           {64, 63},
                                                           {257, 33},
                                                           {513, 127},
                                                           {4097, 3},
                                                           {16000, 1},
                                                           {1, 16000}})
    Case(shape.first, shape.second);
  using namespace jxl::sharper_rows;
  std::vector<float> scratch(kWorkspaceFloats);
  size_t reads = 0, writes = 0;
  auto read = [&](size_t, size_t, size_t, float *) {
    ++reads;
    return false;
  };
  auto write = [&](size_t, size_t, size_t, const float *) {
    ++writes;
    return true;
  };
  assert(
      !Region(17, 19, 0, 0, 9, 10, scratch.data(), 13 * 17 - 1, read, write));
  assert(reads == 0 && writes == 0);
  assert(!Region(17, 19, 0, 0, 9, 10, scratch.data(), scratch.size(), read,
                 write));
  assert(reads == 1 && writes == 0);
  assert(
      !Region(17, 19, 9, 0, 1, 1, scratch.data(), scratch.size(), read, write));
  assert(reads == 1 && writes == 0);
  assert(!Region(std::numeric_limits<size_t>::max(), 1, 0, 0, 1, 1,
                 scratch.data(), scratch.size(), read, write));
  assert(reads == 1 && writes == 0);
  size_t sinks = 0;
  assert(!Region(
      17, 19, 0, 0, 9, 10, scratch.data(), scratch.size(),
      [](size_t, size_t, size_t n, float *p) {
        std::fill(p, p + n, 0.25f);
        return true;
      },
      [&](size_t, size_t, size_t, const float *) {
        ++sinks;
        return false;
      }));
  assert(sinks == 1);
  std::cout << "10 bitwise pinned-reference cases and bounded failure checks "
               "passed\n";
}
