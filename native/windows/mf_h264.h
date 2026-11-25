#ifndef NEXDESK_MF_H264_H_
#define NEXDESK_MF_H264_H_

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

enum { NEXDESK_MF_H264_WIDTH = 640, NEXDESK_MF_H264_HEIGHT = 360 };

struct NexDeskEncodedFrame {
  const uint8_t* bytes;
  size_t size;
  uint32_t key_frame;
};

// Thread-affine encoder. The returned buffer is valid until the next call or close.
void* nexdesk_mf_h264_open(uint32_t fps);
int32_t nexdesk_mf_h264_encode(void* handle, const uint8_t* nv12, size_t size,
                             uint32_t request_key_frame,
                             struct NexDeskEncodedFrame* output);
void nexdesk_mf_h264_close(void* handle);

#ifdef __cplusplus
}
#endif

#endif  // NEXDESK_MF_H264_H_
