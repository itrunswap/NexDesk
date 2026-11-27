#include <CoreVideo/CoreVideo.h>
#include "sdr_pixels.h"

// The low bit denotes full range. Zero denotes an unsupported pixel format.
int nexdesk_cv_color(CVPixelBufferRef image) {
    if (!image) return 0;
    const OSType format = CVPixelBufferGetPixelFormatType(image);
    if (format != kCVPixelFormatType_420YpCbCr8BiPlanarFullRange &&
        format != kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange) return 0;

    // Untagged frames from the NexDesk SDR capturer use libyuv's BT.601 matrix.
    int matrix = ND_601_VIDEO;
    CFTypeRef tag = CVBufferCopyAttachment(image, kCVImageBufferYCbCrMatrixKey, NULL);
    if (tag) {
        if (CFEqual(tag, kCVImageBufferYCbCrMatrix_ITU_R_709_2)) matrix = ND_709_VIDEO;
        if (CFEqual(tag, kCVImageBufferYCbCrMatrix_ITU_R_2020)) matrix = ND_2020_VIDEO;
        CFRelease(tag);
    }
    return matrix + (format == kCVPixelFormatType_420YpCbCr8BiPlanarFullRange);
}

bool nexdesk_cv_to_rgb(CVPixelBufferRef image, int color, bool rgba,
                       uint8_t *out, int stride, int width, int height) {
    if (!image || !out || width < 1 || height < 1 || CVPixelBufferGetPlaneCount(image) != 2 ||
        CVPixelBufferGetWidth(image) != (size_t)width ||
        CVPixelBufferGetHeight(image) != (size_t)height ||
        CVPixelBufferGetHeightOfPlane(image, 0) < (size_t)height ||
        CVPixelBufferGetHeightOfPlane(image, 1) < ((size_t)height + 1) / 2 ||
        CVPixelBufferLockBaseAddress(image, kCVPixelBufferLock_ReadOnly) != kCVReturnSuccess)
        return false;
    const uint8_t *y = CVPixelBufferGetBaseAddressOfPlane(image, 0);
    const uint8_t *uv = CVPixelBufferGetBaseAddressOfPlane(image, 1);
    size_t ys = CVPixelBufferGetBytesPerRowOfPlane(image, 0);
    size_t uvs = CVPixelBufferGetBytesPerRowOfPlane(image, 1);
    bool ok = y && uv && ys >= (size_t)width && uvs >= ((size_t)width + 1) / 2 * 2 &&
              ys <= INT32_MAX && uvs <= INT32_MAX &&
              nexdesk_nv12_to_rgb(y, (int)ys, uv, (int)uvs, out, stride, width, height, color, rgba);
    CVPixelBufferUnlockBaseAddress(image, kCVPixelBufferLock_ReadOnly);
    return ok;
}
