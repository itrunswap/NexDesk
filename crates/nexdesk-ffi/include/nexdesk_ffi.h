#ifndef NEXDESK_FFI_H
#define NEXDESK_FFI_H

#include <stddef.h>
#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

enum NexDeskValidationStatus {
    NEXDESK_VALID = 0,
    NEXDESK_INVALID_DEVICE_ID = 1,
    NEXDESK_INVALID_LAN_ADDRESS = 2,
    NEXDESK_INVALID_LAN_PORT = 3,
    NEXDESK_INVALID_TEXT = 4
};

uint32_t nexdesk_abi_version(void);
uint16_t nexdesk_default_lan_port(void);

/* Text is UTF-8, at most 128 bytes, and need not be NUL-terminated.
 * A null pointer is valid with a zero length. Buffers remain owned by the caller.
 */
int32_t nexdesk_validate_device_id(const uint8_t *id, size_t id_len);
int32_t nexdesk_validate_lan_endpoint(const uint8_t *address,
                                    size_t address_len, const uint8_t *port,
                                    size_t port_len);

enum NexDeskPreviewStatus {
    NEXDESK_PREVIEW_CONNECTING = 1,
    NEXDESK_PREVIEW_STREAMING = 2,
    NEXDESK_PREVIEW_STOPPED = 3,
    NEXDESK_PREVIEW_REJECTED = 4,
    NEXDESK_PREVIEW_HOST_CHANGED = 5,
    NEXDESK_PREVIEW_UNREACHABLE = 6,
    NEXDESK_PREVIEW_MEDIA_FAILED = 7,
    NEXDESK_PREVIEW_BUSY = 8
};

struct NexDeskPreviewFrame {
    const uint8_t *pixels;
    uint32_t width;
    uint32_t height;
};

uint64_t nexdesk_m1_connect(const uint8_t *address, size_t address_len,
                           uint16_t port, const uint8_t *code, size_t code_len,
                           const uint8_t *identity, size_t identity_len);
uint32_t nexdesk_m1_status(uint64_t id);
void nexdesk_m1_disconnect(uint64_t id);
uint64_t nexdesk_m1_frame_sequence(uint64_t id);
uint64_t nexdesk_m1_frame_size(uint64_t id);
uint64_t nexdesk_m1_presented_frames(uint64_t id);
uint64_t nexdesk_m1_texture_wait_us(uint64_t id);
const void *nexdesk_m1_acquire_frame(uint64_t id,
                                     struct NexDeskPreviewFrame *output);
void nexdesk_m1_release_frame(const void *frame);

/* M2 viewer IDs also use the status, disconnect and texture operations above.
 * flags: 1 requests control, 2 remembers the credential, 4 uses a stored credential.
 * JSON outputs are UTF-8 and not NUL terminated. Zero means no output.
 */
uint64_t nexdesk_m2_connect(const uint8_t *address, size_t address_len,
                            uint16_t port, const uint8_t *code, size_t code_len,
                            uint32_t flags);
bool nexdesk_m2_trust(uint64_t id);
bool nexdesk_m2_action(uint64_t id, int32_t kind, uint64_t generation,
                       int32_t x, int32_t y, uint32_t code, bool down,
                       const uint8_t *display, size_t length);
size_t nexdesk_m2_info(uint64_t id, uint8_t *output, size_t capacity);
size_t nexdesk_m2_local(const uint8_t *request, size_t length,
                        uint8_t *output, size_t capacity);

/* M3 uses the M2 control, trust and frame ABI. Request capabilities by bit:
 * 1 system audio, 2 clipboard, 4 file read, 8 file write, 16 display mode,
 * 32 committed text. The host grants a subset after local consent.
 * Requests and replies use bounded UTF-8 JSON. Poll removes a returned reply.
 */
uint64_t nexdesk_m3_connect(const uint8_t *address, size_t address_len,
                            uint16_t port, const uint8_t *code, size_t code_len,
                            uint32_t flags, uint32_t capabilities);
uint64_t nexdesk_m3_request(uint64_t id, const uint8_t *request, size_t length);
size_t nexdesk_m3_poll(uint64_t id, uint64_t request, uint8_t *output, size_t capacity);
size_t nexdesk_m3_info(uint64_t id, uint8_t *output, size_t capacity);

/* ID connection request JSON contains origin, device, code, flags and capabilities. */
uint64_t nexdesk_m4_connect(const uint8_t *request, size_t length);

#ifdef __cplusplus
}
#endif

#endif
