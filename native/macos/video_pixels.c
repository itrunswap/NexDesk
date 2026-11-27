#include <CoreVideo/CoreVideo.h>
#include <stdlib.h>
#include "sdr_pixels.h"

typedef struct {
    CVPixelBufferPoolRef buffers;
    CFDictionaryRef limit;
} NexDeskCapturePool;

static CFDictionaryRef dictionary(const void **keys, const void **values, CFIndex count) {
    return CFDictionaryCreate(NULL, keys, values, count,
                              &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
}

NexDeskCapturePool *nexdesk_cv_capture_pool(int width, int height) {
    if (width <= 0 || height <= 0 || width > 4096 || height > 4096) return NULL;
    NexDeskCapturePool *pool = calloc(1, sizeof(*pool));
    if (!pool) return NULL;
    int format = kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, count = 8;
    CFNumberRef w = CFNumberCreate(NULL, kCFNumberIntType, &width);
    CFNumberRef h = CFNumberCreate(NULL, kCFNumberIntType, &height);
    CFNumberRef f = CFNumberCreate(NULL, kCFNumberIntType, &format);
    CFNumberRef max_buffers = CFNumberCreate(NULL, kCFNumberIntType, &count);
    CFDictionaryRef surface = dictionary(NULL, NULL, 0);
    const void *keys[] = { kCVPixelBufferWidthKey, kCVPixelBufferHeightKey,
                           kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferIOSurfacePropertiesKey };
    const void *values[] = { w, h, f, surface };
    CFDictionaryRef attributes = w && h && f && surface ? dictionary(keys, values, 4) : NULL;
    const void *limit_keys[] = { kCVPixelBufferPoolAllocationThresholdKey };
    const void *limit_values[] = { max_buffers };
    pool->limit = max_buffers ? dictionary(limit_keys, limit_values, 1) : NULL;
    CVReturn rc = attributes && pool->limit
        ? CVPixelBufferPoolCreate(NULL, NULL, attributes, &pool->buffers) : kCVReturnError;
    if (attributes) CFRelease(attributes);
    if (surface) CFRelease(surface);
    if (max_buffers) CFRelease(max_buffers);
    if (f) CFRelease(f);
    if (h) CFRelease(h);
    if (w) CFRelease(w);
    if (rc != kCVReturnSuccess) {
        if (pool->limit) CFRelease(pool->limit);
        free(pool);
        return NULL;
    }
    return pool;
}

void nexdesk_cv_capture_pool_release(NexDeskCapturePool *pool) {
    if (!pool) return;
    CVPixelBufferPoolRelease(pool->buffers);
    CFRelease(pool->limit);
    free(pool);
}

// The returned image has a +1 reference and is locked for writing on success.
CVPixelBufferRef nexdesk_cv_capture_begin(NexDeskCapturePool *pool) {
    CVPixelBufferRef image = NULL;
    if (!pool || CVPixelBufferPoolCreatePixelBufferWithAuxAttributes(
            NULL, pool->buffers, pool->limit, &image) != kCVReturnSuccess) return NULL;
    if (CVPixelBufferLockBaseAddress(image, 0) != kCVReturnSuccess) {
        CVPixelBufferRelease(image);
        return NULL;
    }
    CVBufferSetAttachment(image, kCVImageBufferYCbCrMatrixKey,
        kCVImageBufferYCbCrMatrix_ITU_R_601_4, kCVAttachmentMode_ShouldPropagate);
    CVBufferSetAttachment(image, kCVImageBufferColorPrimariesKey,
        kCVImageBufferColorPrimaries_ITU_R_709_2, kCVAttachmentMode_ShouldPropagate);
    CVBufferSetAttachment(image, kCVImageBufferTransferFunctionKey,
        kCVImageBufferTransferFunction_sRGB, kCVAttachmentMode_ShouldPropagate);
    return image;
}

// The low bit denotes full range. Zero denotes an unsupported pixel format.
int nexdesk_cv_color(CVPixelBufferRef image, bool desktop_sdr) {
    if (!image) return 0;
    const OSType format = CVPixelBufferGetPixelFormatType(image);
    if (format != kCVPixelFormatType_420YpCbCr8BiPlanarFullRange &&
        format != kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange) return 0;

    // Untagged frames from the NexDesk SDR capturer use libyuv's BT.601 matrix.
    int matrix = ND_601_VIDEO;
    CFTypeRef tag = CVBufferCopyAttachment(image, kCVImageBufferYCbCrMatrixKey, NULL);
    if (tag) {
        // VideoToolbox can tag an unlabelled HD H.264 frame as BT.709 without
        // rematrixing its chroma. NexDesk's own SDR desktop encodes BT.601.
        if (!desktop_sdr && CFEqual(tag, kCVImageBufferYCbCrMatrix_ITU_R_709_2)) matrix = ND_709_VIDEO;
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
