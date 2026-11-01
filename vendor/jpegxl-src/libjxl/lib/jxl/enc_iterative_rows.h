// Copyright (c) the JPEG XL Project Authors. All rights reserved.
// Use of this source code is governed by the BSD-style libjxl/LICENSE.
#ifndef LIB_JXL_ENC_ITERATIVE_ROWS_H_
#define LIB_JXL_ENC_ITERATIVE_ROWS_H_
#include <algorithm>
#include <array>
#include <cstddef>
#include <cstdint>
#include <limits>

#include "lib/jxl/enc_sharper_rows.h"

namespace jxl {
namespace iterative_rows {
constexpr size_t kTarget = 2048;
constexpr size_t kMargin = 12;
constexpr size_t kSide = kTarget + 2 * kMargin;
constexpr size_t kMaxStride = kSide + 128;
constexpr size_t kRawStride = 2 * kSide + 12;
constexpr size_t kScratchFloats =
    sharper_rows::kWorkspaceFloats + 11 * kRawStride;
constexpr size_t kScratchCapacityCeiling = kScratchFloats + 4096;
static_assert((3 * kMaxStride * kSide + kScratchCapacityCeiling) *
                  sizeof(float) <
              53 * 1024 * 1024);
// ONE channel at a time: three scalar fields, never three Image3F allocations.
// The actual aligned stride/capacity belongs to the admitted native allocator.
struct Workspace {
  float* initial;
  float* ping;
  float* pong;
  size_t stride;
  size_t rows;
  size_t field_floats;
  float* scratch;
  size_t scratch_floats;
};
struct Rect {
  size_t x0, y0, x1, y1;
};
inline const float* Kernel(size_t x, size_t y) {
  static const float kernel00[25] = {
      -0.01716200f, -0.03452303f, -0.04022174f, -0.02921014f, -0.00624645f,
      -0.03452303f, 0.14111091f,  0.28896755f,  0.00278718f,  -0.01610267f,
      -0.04022174f, 0.28896755f,  0.56661550f,  0.03777607f,  -0.01986694f,
      -0.02921014f, 0.00278718f,  0.03777607f,  -0.03144731f, -0.01185068f,
      -0.00624645f, -0.01610267f, -0.01986694f, -0.01185068f, -0.00213539f,
  };
  static const float kernel01[25] = {
      -0.00624645f, -0.01610267f, -0.01986694f, -0.01185068f, -0.00213539f,
      -0.02921014f, 0.00278718f,  0.03777607f,  -0.03144731f, -0.01185068f,
      -0.04022174f, 0.28896755f,  0.56661550f,  0.03777607f,  -0.01986694f,
      -0.03452303f, 0.14111091f,  0.28896755f,  0.00278718f,  -0.01610267f,
      -0.01716200f, -0.03452303f, -0.04022174f, -0.02921014f, -0.00624645f,
  };
  static const float kernel10[25] = {
      -0.00624645f, -0.02921014f, -0.04022174f, -0.03452303f, -0.01716200f,
      -0.01610267f, 0.00278718f,  0.28896755f,  0.14111091f,  -0.03452303f,
      -0.01986694f, 0.03777607f,  0.56661550f,  0.28896755f,  -0.04022174f,
      -0.01185068f, -0.03144731f, 0.03777607f,  0.00278718f,  -0.02921014f,
      -0.00213539f, -0.01185068f, -0.01986694f, -0.01610267f, -0.00624645f,
  };
  static const float kernel11[25] = {
      -0.00213539f, -0.01185068f, -0.01986694f, -0.01610267f, -0.00624645f,
      -0.01185068f, -0.03144731f, 0.03777607f,  0.00278718f,  -0.02921014f,
      -0.01986694f, 0.03777607f,  0.56661550f,  0.28896755f,  -0.04022174f,
      -0.01610267f, 0.00278718f,  0.28896755f,  0.14111091f,  -0.03452303f,
      -0.00624645f, -0.02921014f, -0.04022174f, -0.03452303f, -0.01716200f,
  };

  return (x & 1) ? ((y & 1) ? kernel11 : kernel10)
                 : ((y & 1) ? kernel01 : kernel00);
}
// Pure filter component. No source, encoded-file or editable-document
// authority. Original float rows are explicit and fallible. A future native
// source adapter MUST split raw RGBA callbacks above2064; this kernel never
// owns an RGBA frame. Workspace allocations must be disjoint and retain their
// admission throughout. Three4-radius passes need12 coarse pixels. Final
// ringing needs initialÃƒÆ’Ã¢â‚¬Å¡Ãƒâ€šÃ‚Â±1; sharper initialization separately
// fetches its originalÃƒÆ’Ã¢â‚¬Å¡Ãƒâ€šÃ‚Â±5/+6 raw support.
template <class ReadRow, class WriteRow>
bool Region(size_t sw, size_t sh, size_t x0, size_t y0, size_t width,
            size_t height, const Workspace& w, ReadRow&& read,
            WriteRow&& write) {
  if (sw == 0 || sh == 0 ||
      sw > static_cast<size_t>(std::numeric_limits<int32_t>::max()) ||
      sh > static_cast<size_t>(std::numeric_limits<int32_t>::max()) ||
      width == 0 || height == 0 || width > kTarget || height > kTarget)
    return false;
  const size_t dw = (sw + 1) / 2, dh = (sh + 1) / 2;
  if (x0 > dw || width > dw - x0 || y0 > dh || height > dh - y0) return false;
  const auto expanded = [&](size_t margin) -> Rect {
    return Rect{x0 > margin ? x0 - margin : 0, y0 > margin ? y0 - margin : 0,
                std::min(dw, x0 + width + margin),
                std::min(dh, y0 + height + margin)};
  };
  const Rect extent = expanded(kMargin);
  if (w.initial == nullptr || w.ping == nullptr || w.pong == nullptr ||
      w.scratch == nullptr || w.initial == w.ping || w.initial == w.pong ||
      w.ping == w.pong || w.stride < extent.x1 - extent.x0 ||
      w.stride > kMaxStride || w.rows < extent.y1 - extent.y0 ||
      w.rows > kSide || w.field_floats < w.stride * w.rows ||
      w.field_floats > kMaxStride * kSide ||
      w.scratch_floats < kScratchFloats ||
      w.scratch_floats > kScratchCapacityCeiling)
    return false;
  const float* owners[4] = {w.initial, w.ping, w.pong, w.scratch};
  const size_t counts[4] = {w.field_floats, w.field_floats, w.field_floats,
                            w.scratch_floats};
  for (size_t i = 0; i < 4; ++i) {
    const uintptr_t first = reinterpret_cast<uintptr_t>(owners[i]);
    const size_t bytes = counts[i] * sizeof(float);
    if (first % alignof(float) != 0 ||
        bytes > std::numeric_limits<uintptr_t>::max() - first)
      return false;
    for (size_t j = 0; j < i; ++j) {
      const uintptr_t other = reinterpret_cast<uintptr_t>(owners[j]);
      const size_t other_bytes = counts[j] * sizeof(float);
      if (other_bytes > std::numeric_limits<uintptr_t>::max() - other ||
          (first < other + other_bytes && other < first + bytes))
        return false;
    }
  }
  const auto offset = [&](size_t x, size_t y) {
    return (y - extent.y0) * w.stride + x - extent.x0;
  };
  for (size_t x = extent.x0; x < extent.x1; x += sharper_rows::kMaxWidth) {
    if (!sharper_rows::Region(
            sw, sh, x, extent.y0,
            std::min(sharper_rows::kMaxWidth, extent.x1 - x),
            extent.y1 - extent.y0, w.scratch, sharper_rows::kWorkspaceFloats,
            read,
            [&](size_t y, size_t start, size_t count, const float* values) {
              std::copy(values, values + count, w.initial + offset(start, y));
              return true;
            }))
      return false;
  }
  for (size_t y = extent.y0; y < extent.y1; ++y)
    std::copy(w.initial + offset(extent.x0, y),
              w.initial + offset(extent.x1, y), w.ping + offset(extent.x0, y));
  float* current = w.ping;
  float* next = w.pong;
  Rect valid = extent;
  float* ring = w.scratch + sharper_rows::kWorkspaceFloats;
  float* original = ring + 10 * kRawStride;
  std::array<size_t, 10> ids;
  const auto value = [&](const float* field, const Rect& bounds, int64_t x,
                         int64_t y, float& result) -> bool {
    x = std::max<int64_t>(0, std::min<int64_t>(dw - 1, x));
    y = std::max<int64_t>(0, std::min<int64_t>(dh - 1, y));
    if (x < static_cast<int64_t>(bounds.x0) ||
        x >= static_cast<int64_t>(bounds.x1) ||
        y < static_cast<int64_t>(bounds.y0) ||
        y >= static_cast<int64_t>(bounds.y1))
      return false;
    result = field[offset(static_cast<size_t>(x), static_cast<size_t>(y))];
    return true;
  };
  for (size_t pass = 0; pass < 3; ++pass) {
    const Rect target = expanded(8 - pass * 4);
    ids.fill(std::numeric_limits<size_t>::max());
    const size_t rx0 = target.x0 * 2 > 4 ? target.x0 * 2 - 4 : 0;
    const size_t rx1 = std::min(sw, (target.x1 - 1) * 2 + 6), span = rx1 - rx0;
    if (span > kRawStride) return false;
    const auto corr_row = [&](size_t sy) -> bool {
      const size_t slot = sy % 10;
      if (ids[slot] == sy) return true;
      if (!read(sy, rx0, span, original)) return false;
      float* corr = ring + slot * kRawStride;
      for (size_t sx = rx0; sx < rx1; ++sx) {
        const float* kernel = Kernel(sx, sy);
        const int64_t cx = sx / 2, cy = sy / 2;
        float minimum = std::numeric_limits<float>::max(),
              maximum = std::numeric_limits<float>::min();
        for (int64_t ky = 0; ky < 5; ++ky)
          for (int64_t kx = 0; kx < 5; ++kx) {
            float v;
            if (!value(current, valid, cx - 2 + kx, cy - 2 + ky, v))
              return false;
            minimum = std::min(minimum, v);
            maximum = std::max(maximum, v);
          }
        float sum = 0;
        for (int64_t ky = 0; ky < 5; ++ky)
          for (int64_t kx = 0; kx < 5; ++kx) {
            float v;
            if (!value(current, valid, cx - 2 + kx, cy - 2 + ky, v))
              return false;
            sum += v * kernel[ky * 5 + kx];
          }
        if (sum < minimum) sum = minimum;
        if (sum > maximum) sum = maximum;
        // Retain both float rounding boundaries of LinComb and ElwiseMul.
        const float difference = 1.0f * original[sx - rx0] + (-1.0f) * sum;
        corr[sx - rx0] = difference * 1.0f;
      }
      ids[slot] = sy;
      return true;
    };
    for (size_t y = target.y0; y < target.y1; ++y) {
      const size_t ry0 = y * 2 > 4 ? y * 2 - 4 : 0,
                   ry1 = std::min(sh, y * 2 + 6);
      for (size_t sy = ry0; sy < ry1; ++sy)
        if (!corr_row(sy)) return false;
      for (size_t x = target.x0; x < target.x1; ++x) {
        const size_t ax0 = x * 2 > 4 ? x * 2 - 4 : 0,
                     ax1 = std::min(sw, x * 2 + 6);
        float sum = 0, weight = 0;
        for (size_t sy = ry0; sy < ry1; ++sy) {
          const float* row = ring + (sy % 10) * kRawStride;
          for (size_t sx = ax0; sx < ax1; ++sx) {
            const int64_t kx = static_cast<int64_t>(x) -
                               static_cast<int64_t>(sx / 2) + 2,
                          ky = static_cast<int64_t>(y) -
                               static_cast<int64_t>(sy / 2) + 2;
            const double deriv = (kx < 0 || kx >= 5 || ky < 0 || ky >= 5)
                                     ? 0
                                     : Kernel(sx, sy)[ky * 5 + kx];
            // Pinned AntiUpsample uses double products with FLOAT sum storage.
            sum += deriv * row[sx - rx0];
            weight += deriv * 1.0f;
          }
        }
        const float correction = sum / weight;
        float before;
        if (!value(current, valid, x, y, before)) return false;
        next[offset(x, y)] = 1.0f * before + 1.0f * correction;
      }
    }
    std::swap(current, next);
    valid = target;
  }
  // Original box mask uses true coarse-image neighbours and partial odd boxes.
  ids.fill(std::numeric_limits<size_t>::max());
  const size_t mx0 = x0 * 2 > 2 ? x0 * 2 - 2 : 0,
               mx1 = std::min(sw, (x0 + width - 1) * 2 + 4), mspan = mx1 - mx0;
  if (mspan > kRawStride) return false;
  const auto original_row = [&](size_t sy) -> bool {
    const size_t slot = sy % 10;
    if (ids[slot] != sy) {
      if (!read(sy, mx0, mspan, ring + slot * kRawStride)) return false;
      ids[slot] = sy;
    }
    return true;
  };
  const auto box = [&](int64_t bx, int64_t by, float& result) -> bool {
    bx = std::max<int64_t>(0, std::min<int64_t>(dw - 1, bx));
    by = std::max<int64_t>(0, std::min<int64_t>(dh - 1, by));
    float sum = 0;
    size_t count = 0;
    for (size_t iy = 0; iy < 2 && 2 * by + iy < sh; ++iy) {
      const size_t sy = 2 * by + iy;
      if (!original_row(sy)) return false;
      const float* row = ring + (sy % 10) * kRawStride;
      for (size_t ix = 0; ix < 2 && 2 * bx + ix < sw; ++ix) {
        sum += row[2 * bx + ix - mx0];
        ++count;
      }
    }
    result = sum / count;
    return true;
  };
  for (size_t y = y0; y < y0 + height; ++y) {
    for (size_t x = x0; x < x0 + width; ++x) {
      float c, left, right, north, south;
      if (!box(x, y, c) || !box(static_cast<int64_t>(x) - 1, y, left) ||
          !box(x + 1, y, right) ||
          !box(x, static_cast<int64_t>(y) - 1, north) || !box(x, y + 1, south))
        return false;
      const float differences[4] = {std::abs(c - left), std::abs(c - right),
                                    std::abs(c - north), std::abs(c - south)};
      float first = std::numeric_limits<float>::max(),
            second = std::numeric_limits<float>::max();
      for (float v : differences) {
        if (v < second) {
          if (v < first) {
            second = first;
            first = v;
          } else {
            second = v;
          }
        }
      }
      float minimum = w.initial[offset(x, y)], maximum = minimum;
      for (int64_t dy = -1; dy <= 1; ++dy)
        for (int64_t dx = -1; dx <= 1; ++dx) {
          const int64_t xx = static_cast<int64_t>(x) + dx,
                        yy = static_cast<int64_t>(y) + dy;
          if (xx < 0 || yy < 0 || xx >= static_cast<int64_t>(dw) ||
              yy >= static_cast<int64_t>(dh))
            continue;
          const float v = w.initial[offset(static_cast<size_t>(xx),
                                           static_cast<size_t>(yy))];
          minimum = std::min(minimum, v);
          maximum = std::max(maximum, v);
        }
      float v = current[offset(x, y)];
      const float a = second * 2;
      const float low = minimum - a, high = maximum + a;
      if (v < low) v = low;
      if (v > high) v = high;
      original[x - x0] = v;
    }
    if (!write(y, x0, width, original)) return false;
  }
  return true;
}
}  // namespace iterative_rows
}  // namespace jxl
#endif  // LIB_JXL_ENC_ITERATIVE_ROWS_H_
