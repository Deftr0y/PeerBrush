// Copyright (c) the JPEG XL Project Authors. All rights reserved.
// BSD-style vendor libjxl/LICENSE applies to the copied pinned filter oracle.
// Scalar component only; no app, decoder, encoded data or dense image fixture.
#define main pinned_sharper_fixture_entry
#include "jxl_sharper_rows.cpp"
#undef main
#include "lib/jxl/enc_iterative_rows.h"
namespace pinned_iterative {
const float kernel00[25] = {
    -0.01716200f, -0.03452303f, -0.04022174f, -0.02921014f, -0.00624645f,
    -0.03452303f, 0.14111091f,  0.28896755f,  0.00278718f,  -0.01610267f,
    -0.04022174f, 0.28896755f,  0.56661550f,  0.03777607f,  -0.01986694f,
    -0.02921014f, 0.00278718f,  0.03777607f,  -0.03144731f, -0.01185068f,
    -0.00624645f, -0.01610267f, -0.01986694f, -0.01185068f, -0.00213539f,
};
const float kernel01[25] = {
    -0.00624645f, -0.01610267f, -0.01986694f, -0.01185068f, -0.00213539f,
    -0.02921014f, 0.00278718f,  0.03777607f,  -0.03144731f, -0.01185068f,
    -0.04022174f, 0.28896755f,  0.56661550f,  0.03777607f,  -0.01986694f,
    -0.03452303f, 0.14111091f,  0.28896755f,  0.00278718f,  -0.01610267f,
    -0.01716200f, -0.03452303f, -0.04022174f, -0.02921014f, -0.00624645f,
};
const float kernel10[25] = {
    -0.00624645f, -0.02921014f, -0.04022174f, -0.03452303f, -0.01716200f,
    -0.01610267f, 0.00278718f,  0.28896755f,  0.14111091f,  -0.03452303f,
    -0.01986694f, 0.03777607f,  0.56661550f,  0.28896755f,  -0.04022174f,
    -0.01185068f, -0.03144731f, 0.03777607f,  0.00278718f,  -0.02921014f,
    -0.00213539f, -0.01185068f, -0.01986694f, -0.01610267f, -0.00624645f,
};
const float kernel11[25] = {
    -0.00213539f, -0.01185068f, -0.01986694f, -0.01610267f, -0.00624645f,
    -0.01185068f, -0.03144731f, 0.03777607f,  0.00278718f,  -0.02921014f,
    -0.01986694f, 0.03777607f,  0.56661550f,  0.28896755f,  -0.04022174f,
    -0.01610267f, 0.00278718f,  0.28896755f,  0.14111091f,  -0.03452303f,
    -0.00624645f, -0.02921014f, -0.04022174f, -0.03452303f, -0.01716200f,
};

const float *Kernel(size_t x, size_t y) {
  return (x & 1) ? ((y & 1) ? kernel11 : kernel10)
                 : ((y & 1) ? kernel01 : kernel00);
}
std::vector<float> Upsample(const std::vector<float> &input, size_t sw,
                            size_t sh) {
  const size_t w = (sw + 1) / 2, h = (sh + 1) / 2;
  std::vector<float> output(sw * sh);
  for (size_t y = 0; y < sh; ++y)
    for (size_t x = 0; x < sw; ++x) {
      const float *kernel = Kernel(x, y);
      float minimum = std::numeric_limits<float>::max(),
            maximum = std::numeric_limits<float>::min();
      for (int64_t ky = 0; ky < 5; ++ky)
        for (int64_t kx = 0; kx < 5; ++kx) {
          int64_t xx = static_cast<int64_t>(x / 2) - 2 + kx,
                  yy = static_cast<int64_t>(y / 2) - 2 + ky;
          xx = std::max<int64_t>(0, std::min<int64_t>(w - 1, xx));
          yy = std::max<int64_t>(0, std::min<int64_t>(h - 1, yy));
          minimum = std::min(minimum, input[yy * w + xx]);
          maximum = std::max(maximum, input[yy * w + xx]);
        }
      float sum = 0;
      for (int64_t ky = 0; ky < 5; ++ky)
        for (int64_t kx = 0; kx < 5; ++kx) {
          int64_t xx = static_cast<int64_t>(x / 2) - 2 + kx,
                  yy = static_cast<int64_t>(y / 2) - 2 + ky;
          xx = std::max<int64_t>(0, std::min<int64_t>(w - 1, xx));
          yy = std::max<int64_t>(0, std::min<int64_t>(h - 1, yy));
          sum += input[yy * w + xx] * kernel[ky * 5 + kx];
        }
      output[y * sw + x] = sum;
      if (output[y * sw + x] < minimum)
        output[y * sw + x] = minimum;
      if (output[y * sw + x] > maximum)
        output[y * sw + x] = maximum;
    }
  return output;
}
std::vector<float> Anti(const std::vector<float> &input, size_t sw, size_t sh) {
  const size_t w = (sw + 1) / 2, h = (sh + 1) / 2;
  std::vector<float> output(w * h);
  for (int64_t y2 = 0; y2 < static_cast<int64_t>(h); ++y2)
    for (int64_t x2 = 0; x2 < static_cast<int64_t>(w); ++x2) {
      const int64_t x0 = std::max<int64_t>(0, x2 * 2 - 4),
                    y0 = std::max<int64_t>(0, y2 * 2 - 4),
                    x1 = std::min<int64_t>(sw, x2 * 2 + 6),
                    y1 = std::min<int64_t>(sh, y2 * 2 + 6);
      float sum = 0;
      for (int64_t y = y0; y < y1; ++y)
        for (int64_t x = x0; x < x1; ++x) {
          const int64_t kx = x2 - x / 2 + 2, ky = y2 - y / 2 + 2;
          const double deriv = kx < 0 || kx >= 5 || ky < 0 || ky >= 5
                                   ? 0
                                   : Kernel(x, y)[ky * 5 + kx];
          sum += deriv * input[y * sw + x];
        }
      output[y2 * w + x2] = sum;
    }
  return output;
}
std::vector<float> OriginalIterative(const std::vector<float> &original,
                                     size_t sw, size_t sh) {
  const size_t w = (sw + 1) / 2, h = (sh + 1) / 2;
  auto initial = Original(original, sw, sh);
  auto down = initial;
  std::vector<float> box(w * h), mask(w * h), weights(sw * sh, 1.0f);
  const auto weights2 = Anti(weights, sw, sh);
  for (size_t y = 0; y < h; ++y)
    for (size_t x = 0; x < w; ++x) {
      float sum = 0;
      size_t count = 0;
      for (size_t iy = 0; iy < 2 && 2 * y + iy < sh; ++iy)
        for (size_t ix = 0; ix < 2 && 2 * x + ix < sw; ++ix) {
          sum += original[(2 * y + iy) * sw + 2 * x + ix];
          ++count;
        }
      box[y * w + x] = sum / count;
    }
  for (size_t y = 0; y < h; ++y)
    for (size_t x = 0; x < w; ++x) {
      const float c = box[y * w + x];
      const float diffs[4] = {
          std::abs(c - box[y * w + (x > 0 ? x - 1 : x)]),
          std::abs(c - box[y * w + (x + 1 < w ? x + 1 : x)]),
          std::abs(c - box[(y > 0 ? y - 1 : y) * w + x]),
          std::abs(c - box[(y + 1 < h ? y + 1 : y) * w + x])};
      float first = std::numeric_limits<float>::max(),
            second = std::numeric_limits<float>::max();
      for (float v : diffs) {
        if (v < second) {
          if (v < first) {
            second = first;
            first = v;
          } else
            second = v;
        }
      }
      mask[y * w + x] = second;
    }
  for (size_t pass = 0; pass < 3; ++pass) {
    auto up = Upsample(down, sw, sh);
    std::vector<float> corr(sw * sh);
    for (size_t i = 0; i < corr.size(); ++i) {
      corr[i] = 1.0f * original[i] + (-1.0f) * up[i];
      corr[i] = corr[i] * weights[i];
    }
    auto corr2 = Anti(corr, sw, sh);
    for (size_t i = 0; i < down.size(); ++i) {
      corr2[i] = corr2[i] / weights2[i];
      down[i] = 1.0f * down[i] + 1.0f * corr2[i];
    }
  }
  for (size_t y = 0; y < h; ++y)
    for (size_t x = 0; x < w; ++x) {
      float minimum = initial[y * w + x], maximum = minimum;
      for (int64_t yi = -1; yi <= 1; ++yi)
        for (int64_t xi = -1; xi <= 1; ++xi) {
          int64_t xx = static_cast<int64_t>(x) + xi,
                  yy = static_cast<int64_t>(y) + yi;
          if (xx < 0 || yy < 0 || xx >= static_cast<int64_t>(w) ||
              yy >= static_cast<int64_t>(h))
            continue;
          minimum = std::min(minimum, initial[yy * w + xx]);
          maximum = std::max(maximum, initial[yy * w + xx]);
        }
      const float a = mask[y * w + x] * 2;
      const float low = minimum - a, high = maximum + a;
      if (down[y * w + x] < low)
        down[y * w + x] = low;
      if (down[y * w + x] > high)
        down[y * w + x] = high;
    }
  return down;
}
} // namespace pinned_iterative
static void RegionCase(const std::vector<float> &input,
                       const std::vector<float> &expected, size_t sw, size_t sh,
                       size_t x, size_t y, size_t width, size_t height) {
  const size_t dw = (sw + 1) / 2, dh = (sh + 1) / 2;
  const size_t bx = x > 12 ? x - 12 : 0, by = y > 12 ? y - 12 : 0,
               ew = std::min(dw, x + width + 12) - bx,
               eh = std::min(dh, y + height + 12) - by;
  const size_t stride = (ew + 15) / 16 * 16;
  std::vector<float> initial(stride * eh), ping(stride * eh), pong(stride * eh),
      scratch(jxl::iterative_rows::kScratchFloats), result(width * height);
  size_t written = 0;
  const jxl::iterative_rows::Workspace work{
      initial.data(), ping.data(),    pong.data(),   stride, eh,
      initial.size(), scratch.data(), scratch.size()};
  assert(jxl::iterative_rows::Region(
      sw, sh, x, y, width, height, work,
      [&](size_t sy, size_t sx, size_t count, float *target) {
        assert(sy < sh && sx + count <= sw);
        assert(count <= jxl::iterative_rows::kRawStride);
        std::copy(input.data() + sy * sw + sx,
                  input.data() + sy * sw + sx + count, target);
        return true;
      },
      [&](size_t dy, size_t dx, size_t count, const float *source) {
        assert(dx == x && dy >= y && dy < y + height && count == width);
        std::copy(source, source + count, result.data() + (dy - y) * width);
        ++written;
        return true;
      }));
  assert(written == height);
  for (size_t row = 0; row < height; ++row) {
    assert(std::memcmp(result.data() + row * width,
                       expected.data() + (row + y) * dw + x,
                       width * sizeof(float)) == 0);
  }
}
int main() {
  size_t regions = 0;
  for (auto shape : std::vector<std::pair<size_t, size_t>>{{1, 1},
                                                           {1, 17},
                                                           {17, 1},
                                                           {17, 19},
                                                           {63, 65},
                                                           {127, 97},
                                                           {257, 33},
                                                           {33, 257},
                                                           {4097, 1},
                                                           {1, 4097}}) {
    const auto sw = shape.first, sh = shape.second;
    assert(sw * sh * sizeof(float) <= 1024 * 1024);
    std::vector<float> input(sw * sh);
    for (size_t i = 0; i < input.size(); ++i) {
      input[i] = (static_cast<int>((i * 137 + 19) % 10000) - 2500) / 10000.0f;
      if (i % 17 == 0)
        input[i] = -0.0f;
    }
    const auto expected = pinned_iterative::OriginalIterative(input, sw, sh);
    const size_t w = (sw + 1) / 2, h = (sh + 1) / 2;
    for (size_t x = 0; x < w; x += 59)
      for (size_t y = 0; y < h; y += 19) {
        RegionCase(input, expected, sw, sh, x, y, std::min<size_t>(59, w - x),
                   std::min<size_t>(19, h - y));
        ++regions;
      }
    if (w >= 2048) {
      RegionCase(input, expected, sw, sh, 0, 0, 2048, 1);
      ++regions;
    }
    if (h >= 2048) {
      RegionCase(input, expected, sw, sh, 0, 0, 1, 2048);
      ++regions;
    }
    uint32_t seed = 42;
    for (size_t i = 0; i < 5; ++i) {
      seed = 1664525u * seed + 1013904223u;
      size_t x = seed % w;
      seed = 1664525u * seed + 1013904223u;
      size_t y = seed % h;
      RegionCase(input, expected, sw, sh, x, y, std::min<size_t>(11, w - x),
                 std::min<size_t>(7, h - y));
      ++regions;
    }
  }
  std::vector<float> a(64), b(64), c(64),
      scratch(jxl::iterative_rows::kScratchFloats);
  size_t reads = 0, writes = 0;
  auto get = [&](size_t, size_t, size_t, float *) {
    ++reads;
    return false;
  };
  auto put = [&](size_t, size_t, size_t, const float *) {
    ++writes;
    return true;
  };
  jxl::iterative_rows::Workspace work{
      a.data(), b.data(), c.data(), 8, 8, 64, scratch.data(), scratch.size()};
  auto short_work = work;
  short_work.field_floats = 1;
  assert(
      !jxl::iterative_rows::Region(17, 19, 0, 0, 3, 3, short_work, get, put));
  assert(reads == 0 && writes == 0);
  assert(!jxl::iterative_rows::Region(7, 7, 0, 0, 3, 3, work, get, put));
  assert(reads == 1 && writes == 0);

  std::vector<float> aa(160), bb(160), cc(160);
  jxl::iterative_rows::Workspace late{
      aa.data(), bb.data(), cc.data(),      16,
      10,        160,       scratch.data(), scratch.size()};
  auto overlapping = late;
  overlapping.pong = overlapping.ping + 1;
  reads = 0;
  writes = 0;
  assert(
      !jxl::iterative_rows::Region(17, 19, 0, 0, 3, 3, overlapping, get, put));
  assert(reads == 0 && writes == 0);
  reads = 0;
  writes = 0;
  assert(!jxl::iterative_rows::Region(
      17, 19, 0, 0, 3, 3, late,
      [&](size_t, size_t, size_t count, float *target) {
        ++reads;
        if (reads == 40)
          return false;
        std::fill(target, target + count, 0.25f);
        return true;
      },
      put));
  assert(reads == 40 && writes == 0);
  reads = 0;
  writes = 0;
  assert(!jxl::iterative_rows::Region(
      17, 19, 0, 0, 3, 3, late,
      [&](size_t, size_t, size_t count, float *target) {
        ++reads;
        std::fill(target, target + count, 0.25f);
        return true;
      },
      [&](size_t, size_t, size_t, const float *) {
        ++writes;
        return false;
      }));
  assert(reads > 40 && writes == 1);
  std::cout << regions
            << " bitwise iterative full-edge/interior/odd/random regions and "
               "failure checks passed\n";
}
