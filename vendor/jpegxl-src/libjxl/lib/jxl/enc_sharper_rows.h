// Copyright (c) the JPEG XL Project Authors. All rights reserved.
//
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.
#ifndef LIB_JXL_ENC_SHARPER_ROWS_H_
#define LIB_JXL_ENC_SHARPER_ROWS_H_
#include <algorithm>
#include <array>
#include <cmath>
#include <cstddef>
#include <cstdint>
#include <limits>

namespace jxl {
namespace sharper_rows {
constexpr size_t kMaxWidth = 2048;
constexpr size_t kRows = 12;
constexpr size_t kStride = 2 * kMaxWidth + 11;
constexpr size_t kWorkspaceFloats = (kRows + 1) * kStride;

// Exact scalar kernel used by the pinned sharper filter. The caller supplies
// admitted workspace and fallible source/sink callbacks; no allocation or I/O
// is hidden here. A successful region is filter work, not encoded-file
// authority. Source rows are absolute image coordinates, including true
// image-edge clamps. read(y, x, count, destination) must fill every requested
// float or return false. write(y, x, count, source) consumes one complete
// output row or returns false.
template <class ReadRow, class WriteRow>
bool Region(size_t source_width, size_t source_height, size_t x0, size_t y0,
            size_t width, size_t height, float* workspace,
            size_t workspace_size, ReadRow&& read, WriteRow&& write) {
  if (source_width == 0 || source_height == 0 ||
      source_width > static_cast<size_t>(std::numeric_limits<int32_t>::max()) ||
      source_height >
          static_cast<size_t>(std::numeric_limits<int32_t>::max()) ||
      width == 0 || height == 0 || width > kMaxWidth ||
      x0 > (source_width + 1) / 2 || width > (source_width + 1) / 2 - x0 ||
      y0 > (source_height + 1) / 2 || height > (source_height + 1) / 2 - y0 ||
      workspace == nullptr)
    return false;
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
  const size_t first_x = x0 * 2 > 5 ? x0 * 2 - 5 : 0;
  const size_t end_x = std::min(source_width, (x0 + width - 1) * 2 + 7);
  const size_t span = end_x - first_x;
  if (span > kStride || workspace_size < (kRows + 1) * span) return false;
  std::array<size_t, kRows> ids;
  ids.fill(std::numeric_limits<size_t>::max());
  std::array<const float*, kRows> row_in;
  float* row_out = workspace + kRows * span;
  const auto clamp_y = [&](int64_t y) -> size_t {
    return static_cast<size_t>(
        std::max<int64_t>(0, std::min<int64_t>(source_height - 1, y)));
  };
  const auto clamp_x = [&](int64_t x) -> size_t {
    return static_cast<size_t>(
        std::max<int64_t>(0, std::min<int64_t>(source_width - 1, x)));
  };
  for (size_t y = y0; y < y0 + height; ++y) {
    for (size_t ky = 0; ky < kRows; ++ky) {
      const size_t iy = clamp_y(static_cast<int64_t>(2 * y + ky) - 5);
      // Consecutive absolute source rows map to distinct slots. Duplicate edge
      // rows reuse the same owned row; the next output row advances by two.
      const size_t slot = iy % kRows;
      float* target = workspace + slot * span;
      if (ids[slot] != iy) {
        if (!read(iy, first_x, span, target)) return false;
        ids[slot] = iy;
      }
      row_in[ky] = target;
    }
    const auto pixel = [&](int64_t x, size_t ky) -> float {
      return row_in[ky][clamp_x(x) - first_x];
    };
    const auto box = [&](int64_t bx, int64_t by) -> float {
      bx = std::max<int64_t>(0, std::min<int64_t>((source_width - 1) / 2, bx));
      by = std::max<int64_t>(0, std::min<int64_t>((source_height - 1) / 2, by));
      float sum = 0;
      size_t count = 0;
      for (size_t iy = 0; iy < 2 && 2 * by + iy < source_height; ++iy) {
        const size_t absolute_y = 2 * by + iy;
        const float* source = workspace + (absolute_y % kRows) * span;
        for (size_t ix = 0; ix < 2 && 2 * bx + ix < source_width; ++ix) {
          sum += source[2 * bx + ix - first_x];
          ++count;
        }
      }
      return sum / count;
    };
    for (size_t local_x = 0; local_x < width; ++local_x) {
      const int64_t x = x0 + local_x;
      const float c = box(x, y);
      const float differences[4] = {
          std::abs(c - box(x - 1, y)), std::abs(c - box(x + 1, y)),
          std::abs(c - box(x, y - 1)), std::abs(c - box(x, y + 1))};
      float mask_min = std::numeric_limits<float>::max();
      float mask_min2 = std::numeric_limits<float>::max();
      for (float v : differences) {
        if (v < mask_min2) {
          if (v < mask_min) {
            mask_min2 = mask_min;
            mask_min = v;
          } else {
            mask_min2 = v;
          }
        }
      }
      // Preserve the pinned minimum initializer and exact accumulation order.
      float minimum = std::numeric_limits<float>::max();
      float maximum = std::numeric_limits<float>::min();
      for (size_t ky = 5; ky + 5 < kRows; ++ky) {
        for (size_t kx = 5; kx + 5 < kRows; ++kx) {
          const float v = pixel(x * 2 + kx - 5, ky);
          minimum = std::min<float>(minimum, v);
          maximum = std::max<float>(maximum, v);
        }
      }
      float sum = 0;
      for (size_t ky = 0; ky < kRows; ++ky) {
        for (size_t kx = 0; kx < kRows; ++kx) {
          sum += pixel(x * 2 + static_cast<int64_t>(kx) - 5, ky) *
                 kernel[ky * kRows + kx];
        }
      }
      row_out[local_x] = sum;
      const float a = mask_min2 * 1;
      const float clip_min = minimum - a;
      const float clip_max = maximum + a;
      if (row_out[local_x] < clip_min)
        row_out[local_x] = clip_min;
      else if (row_out[local_x] > clip_max)
        row_out[local_x] = clip_max;
    }
    if (!write(y, x0, width, row_out)) return false;
  }
  return true;
}
}  // namespace sharper_rows
}  // namespace jxl
#endif  // LIB_JXL_ENC_SHARPER_ROWS_H_
