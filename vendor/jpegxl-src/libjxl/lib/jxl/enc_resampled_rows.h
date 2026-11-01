// Copyright (c) the JPEG XL Project Authors. All rights reserved.
// BSD-style libjxl/LICENSE applies.
#ifndef LIB_JXL_ENC_RESAMPLED_ROWS_H_
#define LIB_JXL_ENC_RESAMPLED_ROWS_H_
#include <algorithm>
#include <cstddef>
#include <utility>

#include "lib/jxl/base/rect.h"
#include "lib/jxl/base/status.h"
#include "lib/jxl/image.h"
namespace jxl {
namespace resampled_rows {
constexpr size_t kBand = 16;
constexpr size_t kWidth = 4192;
constexpr size_t kNativeEdge = 2064;
// Derived row bands only, all allocated through the existing native manager.
// Readers fill complete native pieces; no complete original RGBA frame exists.
// Color uses replicated 8-pixel padding AFTER the original profile->XYB step;
// alpha retains its original unpadded source domain and 2x box semantics.
template <class ColorPiece, class AlphaPiece>
class Bands {
 public:
  Bands(size_t width, size_t height, ColorPiece color, AlphaPiece alpha,
        bool linear)
      : width_(width),
        height_(height),
        padded_width_((width + 7) / 8 * 8),
        padded_height_((height + 7) / 8 * 8),
        color_(std::move(color)),
        alpha_(std::move(alpha)),
        linear_(linear) {}
  Status Init(JxlMemoryManager* manager) {
    JXL_ENSURE(width_ > 0 && height_ > 0 && width_ <= 16000 &&
               height_ <= 16000);
    JXL_ASSIGN_OR_RETURN(color_band_, Image3F::Create(manager, kWidth, kBand));
    JXL_ASSIGN_OR_RETURN(color_piece_,
                         Image3F::Create(manager, kNativeEdge, kBand));
    if (linear_) {
      JXL_ASSIGN_OR_RETURN(linear_piece_,
                           Image3F::Create(manager, kNativeEdge, kBand));
    }
    JXL_ASSIGN_OR_RETURN(alpha_band_, ImageF::Create(manager, kWidth, kBand));
    JXL_ASSIGN_OR_RETURN(alpha_piece_,
                         ImageF::Create(manager, kNativeEdge, kBand));
    return true;
  }
  bool Color(size_t channel, size_t y, size_t x, size_t count, float* target) {
    if (failed_ || channel >= 3 || count == 0 || count > kWidth ||
        x >= padded_width_ || count > padded_width_ - x || y >= padded_height_)
      return Fail();
    const size_t bx = x / 16 * 16, by = y / kBand * kBand,
                 end = std::min(padded_width_, (x + count + 15) / 16 * 16);
    if (!color_valid_ || cx_ != bx || cy_ != by || cw_ < end - bx) {
      const size_t span = end - bx, rows = std::min(kBand, padded_height_ - by);
      if (span > kWidth || bx >= width_ || by >= height_) return Fail();
      const size_t native_width = std::min(span, width_ - bx),
                   native_rows = std::min(rows, height_ - by);
      if (!color_band_.ShrinkTo(span, rows)) return Fail();
      for (size_t at = 0; at < native_width; at += kNativeEdge) {
        const size_t piece = std::min(kNativeEdge, native_width - at);
        if (!color_piece_.ShrinkTo(piece, native_rows)) return Fail();
        if (linear_ && !linear_piece_.ShrinkTo(piece, native_rows))
          return Fail();
        if (!color_(Rect(bx + at, by, piece, native_rows), &color_piece_,
                    linear_ ? &linear_piece_ : nullptr))
          return Fail();
        for (size_t c = 0; c < 3; ++c)
          for (size_t row = 0; row < native_rows; ++row)
            std::copy(color_piece_.ConstPlaneRow(c, row),
                      color_piece_.ConstPlaneRow(c, row) + piece,
                      color_band_.PlaneRow(c, row) + at);
      }
      for (size_t c = 0; c < 3; ++c) {
        for (size_t row = 0; row < native_rows; ++row) {
          float* data = color_band_.PlaneRow(c, row);
          std::fill(data + native_width, data + span, data[native_width - 1]);
        }
        for (size_t row = native_rows; row < rows; ++row)
          std::copy(color_band_.ConstPlaneRow(c, native_rows - 1),
                    color_band_.ConstPlaneRow(c, native_rows - 1) + span,
                    color_band_.PlaneRow(c, row));
      }
      cx_ = bx;
      cy_ = by;
      cw_ = span;
      color_valid_ = true;
    }
    const float* source = color_band_.ConstPlaneRow(channel, y - cy_) + x - cx_;
    std::copy(source, source + count, target);
    return true;
  }
  bool Alpha(size_t y, size_t x, size_t count, float* target) {
    if (failed_ || count == 0 || count > kWidth || x >= width_ ||
        count > width_ - x || y >= height_)
      return Fail();
    const size_t bx = x / 16 * 16, by = y / kBand * kBand,
                 end = std::min(width_, (x + count + 15) / 16 * 16);
    if (!alpha_valid_ || ax_ != bx || ay_ != by || aw_ < end - bx) {
      const size_t span = end - bx, rows = std::min(kBand, height_ - by);
      if (span > kWidth || !alpha_band_.ShrinkTo(span, rows)) return Fail();
      for (size_t at = 0; at < span; at += kNativeEdge) {
        const size_t piece = std::min(kNativeEdge, span - at);
        if (!alpha_piece_.ShrinkTo(piece, rows)) return Fail();
        if (!alpha_(Rect(bx + at, by, piece, rows), &alpha_piece_))
          return Fail();
        for (size_t row = 0; row < rows; ++row)
          std::copy(alpha_piece_.Row(row), alpha_piece_.Row(row) + piece,
                    alpha_band_.Row(row) + at);
      }
      ax_ = bx;
      ay_ = by;
      aw_ = span;
      alpha_valid_ = true;
    }
    const float* source = alpha_band_.Row(y - ay_) + x - ax_;
    std::copy(source, source + count, target);
    return true;
  }

 private:
  bool Fail() {
    failed_ = true;
    return false;
  }
  const size_t width_, height_, padded_width_, padded_height_;
  ColorPiece color_;
  AlphaPiece alpha_;
  const bool linear_;
  Image3F color_band_, color_piece_, linear_piece_;
  ImageF alpha_band_, alpha_piece_;
  size_t cx_ = 0, cy_ = 0, cw_ = 0, ax_ = 0, ay_ = 0, aw_ = 0;
  bool color_valid_ = false, alpha_valid_ = false, failed_ = false;
};
}  // namespace resampled_rows
}  // namespace jxl
#endif
