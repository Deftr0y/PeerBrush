// Copyright (c) the JPEG XL Project Authors. All rights reserved.
//
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

#include "lib/jxl/enc_heuristics.h"
#include "lib/jxl/enc_sharper_rows.h"
#include "lib/jxl/enc_iterative_rows.h"

#include <jxl/cms_interface.h>
#include <jxl/memory_manager.h>

#include <algorithm>
#include <array>
#include <cmath>
#include <cstddef>
#include <cstdint>
#include <cstdlib>
#include <limits>
#include <memory>
#include <numeric>
#include <string>
#include <utility>
#include <vector>

#include "lib/jxl/ac_context.h"
#include "lib/jxl/ac_strategy.h"
#include "lib/jxl/base/common.h"
#include "lib/jxl/base/compiler_specific.h"
#include "lib/jxl/base/data_parallel.h"
#include "lib/jxl/base/override.h"
#include "lib/jxl/base/rect.h"
#include "lib/jxl/base/status.h"
#include "lib/jxl/butteraugli/butteraugli.h"
#include "lib/jxl/chroma_from_luma.h"
#include "lib/jxl/coeff_order.h"
#include "lib/jxl/coeff_order_fwd.h"
#include "lib/jxl/color_encoding_internal.h"
#include "lib/jxl/common.h"
#include "lib/jxl/dct_util.h"
#include "lib/jxl/dec_cache.h"
#include "lib/jxl/dec_group.h"
#include "lib/jxl/dec_noise.h"
#include "lib/jxl/dec_xyb.h"
#include "lib/jxl/enc_ac_strategy.h"
#include "lib/jxl/enc_adaptive_quantization.h"
#include "lib/jxl/enc_cache.h"
#include "lib/jxl/enc_chroma_from_luma.h"
#include "lib/jxl/enc_gaborish.h"
#include "lib/jxl/enc_modular.h"
#include "lib/jxl/enc_noise.h"
#include "lib/jxl/enc_params.h"
#include "lib/jxl/enc_patch_dictionary.h"
#include "lib/jxl/enc_quant_weights.h"
#include "lib/jxl/enc_splines.h"
#include "lib/jxl/epf.h"
#include "lib/jxl/frame_dimensions.h"
#include "lib/jxl/frame_header.h"
#include "lib/jxl/image.h"
#include "lib/jxl/image_metadata.h"
#include "lib/jxl/image_ops.h"
#include "lib/jxl/memory_manager_internal.h"
#include "lib/jxl/passes_state.h"
#include "lib/jxl/quant_weights.h"
#include "lib/jxl/render_pipeline/render_pipeline.h"

namespace jxl {

struct AuxOut;

void FindBestBlockEntropyModel(const CompressParams& cparams, const ImageI& rqf,
                               const AcStrategyImage& ac_strategy,
                               BlockCtxMap* block_ctx_map) {
  if (cparams.decoding_speed_tier >= 1) {
    static constexpr uint8_t kSimpleCtxMap[] = {
        // Cluster all blocks together
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,  //
        1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,  //
        1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,  //
    };
    static_assert(
        3 * kNumOrders == sizeof(kSimpleCtxMap) / sizeof *kSimpleCtxMap,
        "Update simple context map");

    auto bcm = *block_ctx_map;
    bcm.ctx_map.assign(std::begin(kSimpleCtxMap), std::end(kSimpleCtxMap));
    bcm.num_ctxs = 2;
    bcm.num_dc_ctxs = 1;
    return;
  }
  if (cparams.speed_tier >= SpeedTier::kFalcon) {
    return;
  }
  // No need to change context modeling for small images.
  size_t tot = rqf.xsize() * rqf.ysize();
  size_t size_for_ctx_model = (1 << 10) * cparams.butteraugli_distance;
  if (tot < size_for_ctx_model) return;

  struct OccCounters {
    // count the occurrences of each qf value and each strategy type.
    OccCounters(const ImageI& rqf, const AcStrategyImage& ac_strategy) {
      for (size_t y = 0; y < rqf.ysize(); y++) {
        const int32_t* qf_row = rqf.Row(y);
        AcStrategyRow acs_row = ac_strategy.ConstRow(y);
        for (size_t x = 0; x < rqf.xsize(); x++) {
          int ord = kStrategyOrder[acs_row[x].RawStrategy()];
          int qf = qf_row[x] - 1;
          qf_counts[qf]++;
          qf_ord_counts[ord][qf]++;
          ord_counts[ord]++;
        }
      }
    }

    size_t qf_counts[256] = {};
    size_t qf_ord_counts[kNumOrders][256] = {};
    size_t ord_counts[kNumOrders] = {};
  };
  // The OccCounters struct is too big to allocate on the stack.
  std::unique_ptr<OccCounters> counters(new OccCounters(rqf, ac_strategy));

  // Splitting the context model according to the quantization field seems to
  // mostly benefit only large images.
  size_t size_for_qf_split = (1 << 13) * cparams.butteraugli_distance;
  size_t num_qf_segments = tot < size_for_qf_split ? 1 : 2;
  std::vector<uint32_t>& qft = block_ctx_map->qf_thresholds;
  qft.clear();
  // Divide the quant field in up to num_qf_segments segments.
  size_t cumsum = 0;
  size_t next = 1;
  size_t last_cut = 256;
  size_t cut = tot * next / num_qf_segments;
  for (uint32_t j = 0; j < 256; j++) {
    cumsum += counters->qf_counts[j];
    if (cumsum > cut) {
      if (j != 0) {
        qft.push_back(j);
      }
      last_cut = j;
      while (cumsum > cut) {
        next++;
        cut = tot * next / num_qf_segments;
      }
    } else if (next > qft.size() + 1) {
      if (j - 1 == last_cut && j != 0) {
        qft.push_back(j);
      }
    }
  }

  // Count the occurrences of each segment.
  std::vector<size_t> counts(kNumOrders * (qft.size() + 1));
  size_t qft_pos = 0;
  for (size_t j = 0; j < 256; j++) {
    if (qft_pos < qft.size() && j == qft[qft_pos]) {
      qft_pos++;
    }
    for (size_t i = 0; i < kNumOrders; i++) {
      counts[qft_pos + i * (qft.size() + 1)] += counters->qf_ord_counts[i][j];
    }
  }

  // Repeatedly merge the lowest-count pair.
  std::vector<uint8_t> remap((qft.size() + 1) * kNumOrders);
  std::iota(remap.begin(), remap.end(), 0);
  std::vector<uint8_t> clusters(remap);
  size_t nb_clusters =
      Clamp1(static_cast<int>(tot / size_for_ctx_model / 2), 2, 9);
  size_t nb_clusters_chroma =
      Clamp1(static_cast<int>(tot / size_for_ctx_model / 3), 1, 5);
  // This is O(n^2 log n), but n is small.
  while (clusters.size() > nb_clusters) {
    std::sort(clusters.begin(), clusters.end(),
              [&](int a, int b) { return counts[a] > counts[b]; });
    counts[clusters[clusters.size() - 2]] += counts[clusters.back()];
    counts[clusters.back()] = 0;
    remap[clusters.back()] = clusters[clusters.size() - 2];
    clusters.pop_back();
  }
  for (size_t i = 0; i < remap.size(); i++) {
    while (remap[remap[i]] != remap[i]) {
      remap[i] = remap[remap[i]];
    }
  }
  // Relabel starting from 0.
  std::vector<uint8_t> remap_remap(remap.size(), remap.size());
  size_t num = 0;
  for (size_t i = 0; i < remap.size(); i++) {
    if (remap_remap[remap[i]] == remap.size()) {
      remap_remap[remap[i]] = num++;
    }
    remap[i] = remap_remap[remap[i]];
  }
  // Write the block context map.
  auto& ctx_map = block_ctx_map->ctx_map;
  ctx_map = remap;
  ctx_map.resize(remap.size() * 3);
  // for chroma, only use up to nb_clusters_chroma separate block contexts
  // (those for the biggest clusters)
  for (size_t i = remap.size(); i < remap.size() * 3; i++) {
    ctx_map[i] = num + Clamp1(static_cast<int>(remap[i % remap.size()]), 0,
                              static_cast<int>(nb_clusters_chroma) - 1);
  }
  block_ctx_map->num_ctxs =
      *std::max_element(ctx_map.begin(), ctx_map.end()) + 1;
}

namespace {

Status FindBestDequantMatrices(JxlMemoryManager* memory_manager,
                               const CompressParams& cparams,
                               ModularFrameEncoder* modular_frame_encoder,
                               DequantMatrices* dequant_matrices) {
  // TODO(veluca): quant matrices for no-gaborish.
  // TODO(veluca): heuristics for in-bitstream quant tables.
  *dequant_matrices = DequantMatrices();
  if (cparams.max_error_mode || cparams.disable_perceptual_optimizations) {
    constexpr float kMSEWeights[3] = {0.001, 0.001, 0.001};
    const float* wp = cparams.disable_perceptual_optimizations
                          ? kMSEWeights
                          : cparams.max_error;
    // Set numerators of all quantization matrices to constant values.
    float weights[3][1] = {{1.0f / wp[0]}, {1.0f / wp[1]}, {1.0f / wp[2]}};
    DctQuantWeightParams dct_params(weights);
    std::vector<QuantEncoding> encodings(kNumQuantTables,
                                         QuantEncoding::DCT(dct_params));
    JXL_RETURN_IF_ERROR(DequantMatricesSetCustom(dequant_matrices, encodings,
                                                 modular_frame_encoder));
    float dc_weights[3] = {1.0f / wp[0], 1.0f / wp[1], 1.0f / wp[2]};
    JXL_RETURN_IF_ERROR(DequantMatricesSetCustomDC(
        memory_manager, dequant_matrices, dc_weights));
  }
  return true;
}

// Downsamples the image by a factor of 2 with a kernel that's sharper than
// the standard 2x2 box kernel used by DownsampleImage.
// The kernel is optimized against the result of the 2x2 upsampling kernel used
// by the decoder. Ringing is slightly reduced by clamping the values of the
// resulting pixels within certain bounds of a small region in the original
// image.
Status DownsampleImage2_Sharper(const ImageF& input, ImageF* output) {
  JXL_ENSURE(output->xsize() == DivCeil(input.xsize(), 2));
  JXL_ENSURE(output->ysize() == DivCeil(input.ysize(), 2));
  JXL_ASSIGN_OR_RETURN(
      ImageF workspace,
      ImageF::Create(input.memory_manager(),
                     std::min(sharper_rows::kStride, input.xsize()),
                     sharper_rows::kRows + 1));
  for (size_t x = 0; x < output->xsize(); x += sharper_rows::kMaxWidth) {
    JXL_ENSURE(sharper_rows::Region(
        input.xsize(), input.ysize(), x, 0,
        std::min(sharper_rows::kMaxWidth, output->xsize() - x), output->ysize(),
        workspace.Row(0), workspace.PixelsPerRow() * workspace.ysize(),
        [&](size_t y, size_t first, size_t count, float* target) {
          std::copy(input.Row(y) + first, input.Row(y) + first + count, target);
          return true;
        },
        [&](size_t y, size_t first, size_t count, const float* source) {
          std::copy(source, source + count, output->Row(y) + first);
          return true;
        }));
  }
  return true;
}

}  // namespace

Status DownsampleImage2_Sharper(Image3F* opsin) {
  // Allocate extra space to avoid a reallocation when padding.
  JxlMemoryManager* memory_manager = opsin->memory_manager();
  JXL_ASSIGN_OR_RETURN(
      Image3F downsampled,
      Image3F::Create(memory_manager, DivCeil(opsin->xsize(), 2) + kBlockDim,
                      DivCeil(opsin->ysize(), 2) + kBlockDim));
  JXL_RETURN_IF_ERROR(downsampled.ShrinkTo(downsampled.xsize() - kBlockDim,
                                           downsampled.ysize() - kBlockDim));

  for (size_t c = 0; c < 3; c++) {
    JXL_RETURN_IF_ERROR(
        DownsampleImage2_Sharper(opsin->Plane(c), &downsampled.Plane(c)));
  }
  *opsin = std::move(downsampled);
  return true;
}

namespace {

Status DownsampleImage2_Iterative(const ImageF& orig, ImageF* output) {
  JxlMemoryManager* memory_manager = orig.memory_manager();
  const size_t width = DivCeil(orig.xsize(), 2);
  const size_t height = DivCeil(orig.ysize(), 2);
  JXL_RETURN_IF_ERROR(output->ShrinkTo(width, height));
  const size_t field_width = std::min(iterative_rows::kSide, width);
  const size_t field_height = std::min(iterative_rows::kSide, height);
  JXL_ASSIGN_OR_RETURN(ImageF initial, ImageF::Create(memory_manager, field_width, field_height));
  JXL_ASSIGN_OR_RETURN(ImageF ping, ImageF::Create(memory_manager, field_width, field_height));
  JXL_ASSIGN_OR_RETURN(ImageF pong, ImageF::Create(memory_manager, field_width, field_height));
  JXL_ASSIGN_OR_RETURN(ImageF scratch, ImageF::Create(memory_manager, iterative_rows::kScratchFloats, 1));
  const size_t stride = static_cast<size_t>(initial.PixelsPerRow());
  JXL_ENSURE(stride == static_cast<size_t>(ping.PixelsPerRow()));
  JXL_ENSURE(stride == static_cast<size_t>(pong.PixelsPerRow()));
  const iterative_rows::Workspace workspace {
    initial.Row(0), ping.Row(0), pong.Row(0), stride, field_height,
    stride * field_height, scratch.Row(0), static_cast<size_t>(scratch.PixelsPerRow())
  };
  for (size_t y = 0; y < height; y += iterative_rows::kTarget) {
    for (size_t x = 0; x < width; x += iterative_rows::kTarget) {
      JXL_ENSURE(iterative_rows::Region(
          orig.xsize(), orig.ysize(), x, y,
          std::min(iterative_rows::kTarget, width - x),
          std::min(iterative_rows::kTarget, height - y), workspace,
          [&](size_t row, size_t first, size_t count, float* target) {
            std::copy(orig.Row(row) + first, orig.Row(row) + first + count, target);
            return true;
          },
          [&](size_t row, size_t first, size_t count, const float* source) {
            std::copy(source, source + count, output->Row(row) + first);
            return true;
          }));
    }
  }
  return true;
}

}  // namespace

Status DownsampleImage2_Iterative(Image3F* opsin) {
  JxlMemoryManager* memory_manager = opsin->memory_manager();
  // Allocate extra space to avoid a reallocation when padding.
  JXL_ASSIGN_OR_RETURN(
      Image3F downsampled,
      Image3F::Create(memory_manager, DivCeil(opsin->xsize(), 2) + kBlockDim,
                      DivCeil(opsin->ysize(), 2) + kBlockDim));
  JXL_RETURN_IF_ERROR(downsampled.ShrinkTo(downsampled.xsize() - kBlockDim,
                                           downsampled.ysize() - kBlockDim));

  JXL_ASSIGN_OR_RETURN(
      Image3F rgb,
      Image3F::Create(memory_manager, opsin->xsize(), opsin->ysize()));
  OpsinParams opsin_params;  // TODO(user): use the ones that are actually used
  opsin_params.Init(kDefaultIntensityTarget);
  JXL_RETURN_IF_ERROR(
      OpsinToLinear(*opsin, Rect(rgb), nullptr, &rgb, opsin_params));

  JXL_ASSIGN_OR_RETURN(
      ImageF mask,
      ImageF::Create(memory_manager, opsin->xsize(), opsin->ysize()));
  ButteraugliParams butter_params;
  JXL_ASSIGN_OR_RETURN(std::unique_ptr<ButteraugliComparator> butter,
                       ButteraugliComparator::Make(rgb, butter_params));
  JXL_RETURN_IF_ERROR(butter->Mask(&mask));
  JXL_ASSIGN_OR_RETURN(
      ImageF mask_fuzzy,
      ImageF::Create(memory_manager, opsin->xsize(), opsin->ysize()));

  for (size_t c = 0; c < 3; c++) {
    JXL_RETURN_IF_ERROR(
        DownsampleImage2_Iterative(opsin->Plane(c), &downsampled.Plane(c)));
  }
  *opsin = std::move(downsampled);
  return true;
}

StatusOr<Image3F> ReconstructImage(
    const FrameHeader& orig_frame_header, const PassesSharedState& shared,
    const std::vector<std::unique_ptr<ACImage>>& coeffs, ThreadPool* pool) {
  const FrameDimensions& frame_dim = shared.frame_dim;
  JxlMemoryManager* memory_manager = shared.memory_manager;

  FrameHeader frame_header = orig_frame_header;
  frame_header.UpdateFlag(shared.image_features.patches.HasAny(),
                          FrameHeader::kPatches);
  frame_header.UpdateFlag(shared.image_features.splines.HasAny(),
                          FrameHeader::kSplines);
  frame_header.color_transform = ColorTransform::kNone;

  auto metadata = jxl::make_unique<CodecMetadata>();
  *metadata = *frame_header.nonserialized_metadata;
  metadata->m.extra_channel_info.clear();
  metadata->m.num_extra_channels = metadata->m.extra_channel_info.size();
  frame_header.nonserialized_metadata = metadata.get();
  frame_header.extra_channel_upsampling.clear();

  const bool is_gray = shared.metadata->m.color_encoding.IsGray();
  auto dec_state = jxl::make_unique<PassesDecoderState>(memory_manager);
  JXL_RETURN_IF_ERROR(
      dec_state->output_encoding_info.SetFromMetadata(*shared.metadata));
  JXL_RETURN_IF_ERROR(dec_state->output_encoding_info.MaybeSetColorEncoding(
      ColorEncoding::LinearSRGB(is_gray)));
  dec_state->shared = &shared;
  JXL_RETURN_IF_ERROR(dec_state->Init(frame_header));

  ImageBundle decoded(memory_manager, &shared.metadata->m);
  decoded.origin = frame_header.frame_origin;
  JXL_ASSIGN_OR_RETURN(
      Image3F tmp,
      Image3F::Create(memory_manager, frame_dim.xsize, frame_dim.ysize));
  JXL_RETURN_IF_ERROR(decoded.SetFromImage(
      std::move(tmp), dec_state->output_encoding_info.color_encoding));

  PassesDecoderState::PipelineOptions options;
  options.use_slow_render_pipeline = false;
  options.coalescing = false;
  options.render_spotcolors = false;
  options.render_noise = true;

  JXL_RETURN_IF_ERROR(dec_state->PreparePipeline(
      frame_header, &shared.metadata->m, &decoded, options));

  AlignedArray<GroupDecCache> group_dec_caches;
  const auto allocate_storage = [&](const size_t num_threads) -> Status {
    JXL_RETURN_IF_ERROR(
        dec_state->render_pipeline->PrepareForThreads(num_threads,
                                                      /*use_group_ids=*/false));
    JXL_ASSIGN_OR_RETURN(group_dec_caches, AlignedArray<GroupDecCache>::Create(
                                               memory_manager, num_threads));
    return true;
  };
  const auto process_group = [&](const uint32_t group_index,
                                 const size_t thread) -> Status {
    if (frame_header.loop_filter.epf_iters > 0) {
      JXL_RETURN_IF_ERROR(ComputeSigma(frame_header.loop_filter,
                                       frame_dim.BlockGroupRect(group_index),
                                       dec_state.get()));
    }
    RenderPipelineInput input =
        dec_state->render_pipeline->GetInputBuffers(group_index, thread);
    JXL_RETURN_IF_ERROR(DecodeGroupForRoundtrip(
        frame_header, coeffs, group_index, dec_state.get(),
        &group_dec_caches[thread], thread, input, nullptr, nullptr));
    if ((frame_header.flags & FrameHeader::kNoise) != 0) {
      PrepareNoiseInput(*dec_state, shared.frame_dim, frame_header, group_index,
                        thread);
    }
    JXL_RETURN_IF_ERROR(input.Done());
    return true;
  };
  JXL_RETURN_IF_ERROR(RunOnPool(pool, 0, frame_dim.num_groups, allocate_storage,
                                process_group, "ReconstructImage"));
  return std::move(*decoded.color());
}

float ComputeBlockL2Distance(const Image3F& a, const Image3F& b,
                             const ImageF& mask1x1, size_t by, size_t bx) {
  Rect rect(bx * kBlockDim, by * kBlockDim, kBlockDim, kBlockDim, a.xsize(),
            a.ysize());
  float err2[3] = {0.0f};
  for (size_t y = 0; y < rect.ysize(); ++y) {
    const float* row_a[3] = {
        rect.ConstPlaneRow(a, 0, y),
        rect.ConstPlaneRow(a, 1, y),
        rect.ConstPlaneRow(a, 2, y),
    };
    const float* row_b[3] = {
        rect.ConstPlaneRow(b, 0, y),
        rect.ConstPlaneRow(b, 1, y),
        rect.ConstPlaneRow(b, 2, y),
    };
    const float* row_mask = rect.ConstRow(mask1x1, y);
    for (size_t x = 0; x < rect.xsize(); ++x) {
      float mask = row_mask[x];
      float mask2 = mask * mask;
      for (int i = 0; i < 3; ++i) {
        float diff = row_a[i][x] - row_b[i][x];
        err2[i] += mask2 * diff * diff;
      }
    }
  }
  static const double kW[] = {
      12.339445295782363,
      1.0,
      0.2,
  };
  float retval = kW[0] * err2[0] + kW[1] * err2[1] + kW[2] * err2[2];
  return retval;
}

Status ComputeARHeuristics(const FrameHeader& frame_header,
                           PassesEncoderState* enc_state,
                           const Image3F& orig_opsin, const Rect& rect,
                           ThreadPool* pool) {
  const CompressParams& cparams = enc_state->cparams;
  PassesSharedState& shared = enc_state->shared;
  const FrameDimensions& frame_dim = shared.frame_dim;
  const ImageF& initial_quant_masking1x1 = enc_state->initial_quant_masking1x1;
  ImageB& epf_sharpness = shared.epf_sharpness;
  JxlMemoryManager* memory_manager = enc_state->memory_manager();

  float clamped_butteraugli = std::min(5.0f, cparams.butteraugli_distance);
  if (cparams.butteraugli_distance < kMinButteraugliForDynamicAR ||
      cparams.speed_tier > SpeedTier::kWombat ||
      frame_header.loop_filter.epf_iters == 0) {
    FillPlane(static_cast<uint8_t>(4), &epf_sharpness, Rect(epf_sharpness));
    return true;
  }

  std::vector<uint8_t> epf_steps;
  if (cparams.butteraugli_distance > 4.5f) {
    epf_steps.push_back(0);
    epf_steps.push_back(4);
  } else {
    epf_steps.push_back(0);
    epf_steps.push_back(2);
    epf_steps.push_back(7);
  }
  static const int kNumEPFVals = 8;
  size_t epf_steps_lut[kNumEPFVals] = {0};
  {
    for (size_t i = 0; i < epf_steps.size(); ++i) {
      epf_steps_lut[epf_steps[i]] = i;
    }
  }
  std::array<ImageF, kNumEPFVals> error_images;
  for (uint8_t val : epf_steps) {
    FillPlane(val, &epf_sharpness, Rect(epf_sharpness));
    JXL_ASSIGN_OR_RETURN(
        Image3F decoded,
        ReconstructImage(frame_header, shared, enc_state->coeffs, pool));
    JXL_ASSIGN_OR_RETURN(error_images[val],
                         ImageF::Create(memory_manager, frame_dim.xsize_blocks,
                                        frame_dim.ysize_blocks));
    for (size_t by = 0; by < frame_dim.ysize_blocks; by++) {
      float* error_row = error_images[val].Row(by);
      for (size_t bx = 0; bx < frame_dim.xsize_blocks; bx++) {
        error_row[bx] = ComputeBlockL2Distance(
            orig_opsin, decoded, initial_quant_masking1x1, by, bx);
      }
    }
  }
  std::vector<std::vector<size_t>> histo(9, std::vector<size_t>(kNumEPFVals));
  std::vector<size_t> totals(9, 1);
  static const float kFavorNoSmoothing = 0.99;
  for (size_t by = 0; by < frame_dim.ysize_blocks; by++) {
    uint8_t* JXL_RESTRICT out_row = epf_sharpness.Row(by);
    uint8_t* JXL_RESTRICT prev_row = epf_sharpness.Row(by > 0 ? by - 1 : 0);
    for (size_t bx = 0; bx < frame_dim.xsize_blocks; bx++) {
      uint8_t best_val = 0;
      float best_error = std::numeric_limits<float>::max();
      uint8_t top_val = by > 0 ? prev_row[bx] : 0;
      uint8_t left_val = bx > 0 ? out_row[bx - 1] : 0;
      float top_error = error_images[top_val].Row(by)[bx];
      float left_error = error_images[left_val].Row(by)[bx];
      for (uint8_t val : epf_steps) {
        float error = error_images[val].Row(by)[bx];
        if (val == 0) {
          error *= kFavorNoSmoothing;
        }
        if (error < best_error) {
          best_val = val;
          best_error = error;
        }
      }
      if (best_error < std::min(top_error, left_error)) {
        out_row[bx] = best_val;
      } else if (top_error < left_error) {
        out_row[bx] = top_val;
      } else {
        out_row[bx] = left_val;
      }
      int context = epf_steps_lut[top_val] * 3 + epf_steps_lut[left_val];
      ++histo[context][out_row[bx]];
      ++totals[context];
    }
  }
  const float c3base = 0.98017198824148288;
  const float c3clamp = 0.85970338919928291;
  const float c3 = std::max(c3clamp, std::pow(c3base, clamped_butteraugli));
  static const float c5 = 0.1087690359555803;
  float mul[3 * 3 * 3] = {0};
  for (uint8_t top_val : epf_steps) {
    for (uint8_t left_val : epf_steps) {
      for (uint8_t val : epf_steps) {
        int context = epf_steps_lut[top_val] * 3 + epf_steps_lut[left_val];
        const auto& ctx_histo = histo[context];
        const int mulix = epf_steps_lut[val] + 3 * context;
        mul[mulix] =
            1.0 / (1.0 + c5 * std::log1p(ctx_histo[val] / totals[context]) /
                             clamped_butteraugli);
        if (val == 0) {
          mul[mulix] *= c3;
        }
      }
    }
  }
  for (size_t by = 0; by < frame_dim.ysize_blocks; by++) {
    uint8_t* JXL_RESTRICT out_row = epf_sharpness.Row(by);
    uint8_t* JXL_RESTRICT prev_row = epf_sharpness.Row(by > 0 ? by - 1 : 0);
    for (size_t bx = 0; bx < frame_dim.xsize_blocks; bx++) {
      uint8_t best_val = 0;
      float best_error = std::numeric_limits<float>::max();
      uint8_t top_val = by > 0 ? prev_row[bx] : 0;
      uint8_t left_val = bx > 0 ? out_row[bx - 1] : 0;
      int context = epf_steps_lut[top_val] * 3 + epf_steps_lut[left_val];
      for (uint8_t val : epf_steps) {
        int mulix = epf_steps_lut[val] + 3 * context;
        float error = error_images[val].Row(by)[bx] * mul[mulix];
        if (error < best_error) {
          best_val = val;
          best_error = error;
        }
      }
      out_row[bx] = best_val;
    }
  }

  return true;
}

Status LossyFrameHeuristics(const FrameHeader& frame_header,
                            PassesEncoderState* enc_state,
                            ModularFrameEncoder* modular_frame_encoder,
                            const Image3F* linear, Image3F* opsin,
                            const Rect& rect, const JxlCmsInterface& cms,
                            ThreadPool* pool, AuxOut* aux_out) {
  const CompressParams& cparams = enc_state->cparams;
  const bool streaming_mode = enc_state->streaming_mode;
  const bool initialize_global_state = enc_state->initialize_global_state;
  PassesSharedState& shared = enc_state->shared;
  const FrameDimensions& frame_dim = shared.frame_dim;
  ImageFeatures& image_features = shared.image_features;
  DequantMatrices& matrices = shared.matrices;
  Quantizer& quantizer = shared.quantizer;
  ImageF& initial_quant_masking1x1 = enc_state->initial_quant_masking1x1;
  ImageI& raw_quant_field = shared.raw_quant_field;
  ColorCorrelationMap& cmap = shared.cmap;
  AcStrategyImage& ac_strategy = shared.ac_strategy;
  BlockCtxMap& block_ctx_map = shared.block_ctx_map;
  JxlMemoryManager* memory_manager = enc_state->memory_manager();

  // Find and subtract splines.
  bool override_splines = cparams.custom_splines.HasAny();
  if (override_splines) {
    image_features.splines.SetData(cparams.custom_splines);
  }
  if (!streaming_mode && cparams.speed_tier <= SpeedTier::kSquirrel) {
    if (!override_splines) {
      image_features.splines = FindSplines(*opsin);
    }
    JXL_RETURN_IF_ERROR(image_features.splines.InitializeDrawCache(
        opsin->xsize(), opsin->ysize(), cmap.base()));
    image_features.splines.SubtractFrom(opsin);
  }

  // Find and subtract patches/dots.
  if (!streaming_mode &&
      ApplyOverride(cparams.patches,
                    cparams.speed_tier <= SpeedTier::kSquirrel)) {
    JXL_RETURN_IF_ERROR(
        FindBestPatchDictionary(*opsin, enc_state, cms, pool, aux_out));
    JXL_RETURN_IF_ERROR(
        PatchDictionaryEncoder::SubtractFrom(image_features.patches, opsin));
  }

  const float quant_dc = InitialQuantDC(cparams.butteraugli_distance);

  // TODO(veluca): we can now run all the code from here to FindBestQuantizer
  // (excluded) one rect at a time. Do that.

  // Dependency graph:
  //
  // input: either XYB or input image
  //
  // input image -> XYB [optional]
  // XYB -> initial quant field
  // XYB -> Gaborished XYB
  // Gaborished XYB -> CfL1
  // initial quant field, Gaborished XYB, CfL1 -> ACS
  // initial quant field, ACS, Gaborished XYB -> EPF control field
  // initial quant field -> adjusted initial quant field
  // adjusted initial quant field, ACS -> raw quant field
  // raw quant field, ACS, Gaborished XYB -> CfL2
  //
  // output: Gaborished XYB, CfL, ACS, raw quant field, EPF control field.

  AcStrategyHeuristics acs_heuristics(memory_manager, cparams);
  CfLHeuristics cfl_heuristics(memory_manager);
  ImageF initial_quant_field;
  ImageF initial_quant_masking;

  // Compute an initial estimate of the quantization field.
  // Call InitialQuantField only in Hare mode or slower. Otherwise, rely
  // on simple heuristics in FindBestAcStrategy, or set a constant for Falcon
  // mode.
  if (cparams.speed_tier > SpeedTier::kHare ||
      cparams.disable_perceptual_optimizations) {
    JXL_ASSIGN_OR_RETURN(initial_quant_field,
                         ImageF::Create(memory_manager, frame_dim.xsize_blocks,
                                        frame_dim.ysize_blocks));
    JXL_ASSIGN_OR_RETURN(initial_quant_masking,
                         ImageF::Create(memory_manager, frame_dim.xsize_blocks,
                                        frame_dim.ysize_blocks));
    float q = 0.79 / cparams.butteraugli_distance;
    FillImage(q, &initial_quant_field);
    float masking = 1.0f / (q + 0.001f);
    FillImage(masking, &initial_quant_masking);
    if (cparams.disable_perceptual_optimizations) {
      JXL_ASSIGN_OR_RETURN(
          initial_quant_masking1x1,
          ImageF::Create(memory_manager, frame_dim.xsize, frame_dim.ysize));
      FillImage(masking, &initial_quant_masking1x1);
    }
    quantizer.ComputeGlobalScaleAndQuant(quant_dc, q, 0);
  } else {
    // Call this here, as it relies on pre-gaborish values.
    float butteraugli_distance_for_iqf = cparams.butteraugli_distance;
    if (!frame_header.loop_filter.gab) {
      butteraugli_distance_for_iqf *= 0.62f;
    }
    JXL_ASSIGN_OR_RETURN(
        initial_quant_field,
        InitialQuantField(butteraugli_distance_for_iqf, *opsin, rect, pool,
                          1.0f, &initial_quant_masking,
                          &initial_quant_masking1x1));
    float q = 0.39 / cparams.butteraugli_distance;
    quantizer.ComputeGlobalScaleAndQuant(quant_dc, q, 0);
  }

  // TODO(veluca): do something about animations.

  // Apply inverse-gaborish.
  if (frame_header.loop_filter.gab) {
    // Changing the weight here to 0.99f would help to reduce ringing in
    // generation loss.
    float weight[3] = {
        1.0f,
        1.0f,
        1.0f,
    };
    JXL_RETURN_IF_ERROR(GaborishInverse(opsin, rect, weight, pool));
  }

  if (initialize_global_state) {
    JXL_RETURN_IF_ERROR(FindBestDequantMatrices(
        memory_manager, cparams, modular_frame_encoder, &matrices));
  }

  JXL_RETURN_IF_ERROR(cfl_heuristics.Init(rect));
  JXL_RETURN_IF_ERROR(acs_heuristics.Init(*opsin, rect, initial_quant_field,
                                          initial_quant_masking,
                                          initial_quant_masking1x1, &matrices));

  auto process_tile = [&](const uint32_t tid, const size_t thread) -> Status {
    size_t n_enc_tiles = DivCeil(frame_dim.xsize_blocks, kEncTileDimInBlocks);
    size_t tx = tid % n_enc_tiles;
    size_t ty = tid / n_enc_tiles;
    size_t by0 = ty * kEncTileDimInBlocks;
    size_t by1 =
        std::min((ty + 1) * kEncTileDimInBlocks, frame_dim.ysize_blocks);
    size_t bx0 = tx * kEncTileDimInBlocks;
    size_t bx1 =
        std::min((tx + 1) * kEncTileDimInBlocks, frame_dim.xsize_blocks);
    Rect r(bx0, by0, bx1 - bx0, by1 - by0);

    // For speeds up to Wombat, we only compute the color correlation map
    // once we know the transform type and the quantization map.
    if (cparams.speed_tier <= SpeedTier::kSquirrel) {
      JXL_RETURN_IF_ERROR(cfl_heuristics.ComputeTile(
          r, *opsin, rect, matrices,
          /*ac_strategy=*/nullptr,
          /*raw_quant_field=*/nullptr,
          /*quantizer=*/nullptr, /*fast=*/false, thread, &cmap));
    }

    // Choose block sizes.
    JXL_RETURN_IF_ERROR(
        acs_heuristics.ProcessRect(r, cmap, &ac_strategy, thread));

    // Always set the initial quant field, so we can compute the CfL map with
    // more accuracy. The initial quant field might change in slower modes, but
    // adjusting the quant field with butteraugli when all the other encoding
    // parameters are fixed is likely a more reliable choice anyway.
    JXL_RETURN_IF_ERROR(AdjustQuantField(
        ac_strategy, r, cparams.butteraugli_distance, &initial_quant_field));
    quantizer.SetQuantFieldRect(initial_quant_field, r, &raw_quant_field);

    // Compute a non-default CfL map if we are at Hare speed, or slower.
    if (cparams.speed_tier <= SpeedTier::kHare) {
      JXL_RETURN_IF_ERROR(cfl_heuristics.ComputeTile(
          r, *opsin, rect, matrices, &ac_strategy, &raw_quant_field, &quantizer,
          /*fast=*/cparams.speed_tier >= SpeedTier::kWombat, thread, &cmap));
    }
    return true;
  };
  size_t num_tiles = DivCeil(frame_dim.xsize_blocks, kEncTileDimInBlocks) *
                     DivCeil(frame_dim.ysize_blocks, kEncTileDimInBlocks);
  const auto prepare = [&](const size_t num_threads) -> Status {
    JXL_RETURN_IF_ERROR(acs_heuristics.PrepareForThreads(num_threads));
    JXL_RETURN_IF_ERROR(cfl_heuristics.PrepareForThreads(num_threads));
    return true;
  };
  JXL_RETURN_IF_ERROR(
      RunOnPool(pool, 0, num_tiles, prepare, process_tile, "Enc Heuristics"));

  JXL_RETURN_IF_ERROR(acs_heuristics.Finalize(frame_dim, ac_strategy, aux_out));

  // Refine quantization levels.
  if (!streaming_mode && !cparams.disable_perceptual_optimizations) {
    ImageB& epf_sharpness = shared.epf_sharpness;
    FillPlane(static_cast<uint8_t>(4), &epf_sharpness, Rect(epf_sharpness));
    JXL_RETURN_IF_ERROR(FindBestQuantizer(frame_header, linear, *opsin,
                                          initial_quant_field, enc_state, cms,
                                          pool, aux_out));
  }

  // Choose a context model that depends on the amount of quantization for AC.
  if (cparams.speed_tier < SpeedTier::kFalcon && initialize_global_state) {
    FindBestBlockEntropyModel(cparams, raw_quant_field, ac_strategy,
                              &block_ctx_map);
  }
  return true;
}

}  // namespace jxl
