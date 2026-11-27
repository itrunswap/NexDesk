#include "sdr_pixels.h"
#include <libyuv/convert_argb.h>
#include <stddef.h>

// libyuv's ARGB writes BGRA bytes on little-endian desktops. For
// RGBA output use its mirrored coefficients and exchange the U/V arguments.
static const struct YuvConstants *coefficients(int color, bool rgba) {
    switch (color) {
        case ND_601_VIDEO: return rgba ? &kYvuI601Constants : &kYuvI601Constants;
        case ND_601_FULL: return rgba ? &kYvuJPEGConstants : &kYuvJPEGConstants;
        case ND_709_VIDEO: return rgba ? &kYvuH709Constants : &kYuvH709Constants;
        case ND_709_FULL: return rgba ? &kYvuF709Constants : &kYuvF709Constants;
        case ND_2020_VIDEO: return rgba ? &kYvu2020Constants : &kYuv2020Constants;
        case ND_2020_FULL: return rgba ? &kYvuV2020Constants : &kYuvV2020Constants;
        default: return NULL;
    }
}

bool nexdesk_nv12_to_rgb(const uint8_t *y, int ys, const uint8_t *uv, int uvs,
                         uint8_t *out, int stride, int width, int height, int color, bool rgba) {
    const struct YuvConstants *matrix = coefficients(color, rgba);
    return matrix && (rgba ? NV21ToARGBMatrix : NV12ToARGBMatrix)(
        y, ys, uv, uvs, out, stride, matrix, width, height) == 0;
}

void nexdesk_planar_to_rgb(const uint8_t *y, int ys, const uint8_t *u, int us,
                           const uint8_t *v, int vs, uint8_t *out, int stride,
                           int width, int height, int color, bool rgba, int sampling) {
    const struct YuvConstants *matrix = coefficients(color, rgba);
    if (!matrix) return;
    (sampling == 444 ? I444ToARGBMatrix : sampling == 422 ? I422ToARGBMatrix : I420ToARGBMatrix)(
                     y, ys, rgba ? v : u, rgba ? vs : us, rgba ? u : v,
                     rgba ? us : vs, out, stride, matrix, width, height);
}
