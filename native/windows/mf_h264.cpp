#include "mf_h264.h"

#include <windows.h>
#include <codecapi.h>
#include <icodecapi.h>
#include <mfapi.h>
#include <mferror.h>
#include <mfidl.h>
#include <wrl/client.h>

#include <algorithm>
#include <chrono>
#include <cstring>
#include <memory>
#include <thread>
#include <vector>

namespace {
using Microsoft::WRL::ComPtr;

constexpr UINT32 kWidth = NEXDESK_MF_H264_WIDTH;
constexpr UINT32 kHeight = NEXDESK_MF_H264_HEIGHT;
constexpr DWORD kNv12Size = kWidth * kHeight * 3 / 2;
constexpr DWORD kMaxEncodedSize = 2 * 1024 * 1024;
constexpr UINT32 kBitrate = 2'000'000;

class Encoder {
 public:
  ~Encoder() {
    if (transform_) {
      transform_->ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
      transform_->ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
    }
    events_.Reset();
    codec_.Reset();
    transform_.Reset();
    if (activation_) activation_->ShutdownObject();
    activation_.Reset();
    if (media_foundation_) MFShutdown();
    if (com_) CoUninitialize();
  }

  bool Start(UINT32 fps) {
    if (fps < 1 || fps > 60 || FAILED(CoInitializeEx(nullptr, COINIT_MULTITHREADED))) {
      return false;
    }
    com_ = true;
    if (FAILED(MFStartup(MF_VERSION))) return false;
    media_foundation_ = true;
    fps_ = fps;
    MFT_REGISTER_TYPE_INFO input{MFMediaType_Video, MFVideoFormat_NV12};
    MFT_REGISTER_TYPE_INFO output{MFMediaType_Video, MFVideoFormat_H264};
    IMFActivate** list = nullptr;
    UINT32 count = 0;
    if (FAILED(MFTEnumEx(MFT_CATEGORY_VIDEO_ENCODER,
                         MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
                         &input, &output, &list, &count))) {
      return false;
    }
    bool ready = false;
    for (UINT32 index = 0; index < count; ++index) {
      if (!ready) {
        activation_ = list[index];
        if (SUCCEEDED(activation_->ActivateObject(IID_PPV_ARGS(&transform_)))) {
          ready = Configure();
        }
        if (!ready) {
          events_.Reset();
          codec_.Reset();
          transform_.Reset();
          activation_->ShutdownObject();
          activation_.Reset();
        }
      }
      list[index]->Release();
    }
    CoTaskMemFree(list);
    return ready;
  }

  int Encode(const uint8_t* pixels, size_t size, bool key_frame,
             NexDeskEncodedFrame* output) {
    if (!pixels || size != kNv12Size || !output) return -1;
    *output = {};
    if (key_frame && codec_) {
      VARIANT force{};
      force.vt = VT_UI4;
      force.ulVal = 1;
      codec_->SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &force);
    }
    if (!events_) {
      if (FAILED(Submit(pixels))) return -1;
      return Receive(output);
    }

    bool submitted = false;
    const auto deadline = std::chrono::steady_clock::now() + std::chrono::milliseconds(20);
    while (std::chrono::steady_clock::now() < deadline) {
      if (needs_input_ && !submitted) {
        if (FAILED(Submit(pixels))) return -1;
        submitted = true;
        needs_input_ = false;
      }
      ComPtr<IMFMediaEvent> event;
      const auto result = events_->GetEvent(MF_EVENT_FLAG_NO_WAIT, &event);
      if (result == MF_E_NO_EVENTS_AVAILABLE) {
        std::this_thread::sleep_for(std::chrono::milliseconds(1));
        continue;
      }
      if (FAILED(result)) return -1;
      MediaEventType type = MEUnknown;
      HRESULT status = S_OK;
      if (FAILED(event->GetStatus(&status)) || FAILED(status) ||
          FAILED(event->GetType(&type))) {
        return -1;
      }
      if (type == METransformNeedInput) needs_input_ = true;
      if (type == METransformHaveOutput) {
        const int ready = Receive(output);
        if (ready != 0) return ready;
      }
    }
    return 0;
  }

 private:
  bool Configure() {
    ComPtr<IMFAttributes> attributes;
    if (FAILED(transform_->GetAttributes(&attributes))) return false;
    UINT32 async = 0;
    attributes->GetUINT32(MF_TRANSFORM_ASYNC, &async);
    if (async && (FAILED(attributes->SetUINT32(MF_TRANSFORM_ASYNC_UNLOCK, TRUE)) ||
                  FAILED(transform_.As(&events_)))) {
      return false;
    }
    attributes->SetUINT32(MF_LOW_LATENCY, TRUE);
    if (SUCCEEDED(transform_.As(&codec_))) {
      VARIANT no_b_frames{};
      no_b_frames.vt = VT_UI4;
      codec_->SetValue(&CODECAPI_AVEncMPVDefaultBPictureCount, &no_b_frames);
    }
    const auto ids = transform_->GetStreamIDs(1, &input_id_, 1, &output_id_);
    if (ids == E_NOTIMPL) input_id_ = output_id_ = 0;
    else if (FAILED(ids)) return false;
    return SetType(true) && SetType(false) &&
           SUCCEEDED(transform_->ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)) &&
           SUCCEEDED(transform_->ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0));
  }

  bool SetType(bool output) {
    ComPtr<IMFMediaType> type;
    if (FAILED(MFCreateMediaType(&type)) ||
        FAILED(type->SetGUID(MF_MT_MAJOR_TYPE, MFMediaType_Video)) ||
        FAILED(type->SetGUID(MF_MT_SUBTYPE, output ? MFVideoFormat_H264 : MFVideoFormat_NV12)) ||
        FAILED(MFSetAttributeSize(type.Get(), MF_MT_FRAME_SIZE, kWidth, kHeight)) ||
        FAILED(MFSetAttributeRatio(type.Get(), MF_MT_FRAME_RATE, fps_, 1)) ||
        FAILED(MFSetAttributeRatio(type.Get(), MF_MT_PIXEL_ASPECT_RATIO, 1, 1)) ||
        FAILED(type->SetUINT32(MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive))) {
      return false;
    }
    if (!output) return SUCCEEDED(transform_->SetInputType(input_id_, type.Get(), 0));
    return SUCCEEDED(type->SetUINT32(MF_MT_AVG_BITRATE, kBitrate)) &&
           SUCCEEDED(type->SetUINT32(MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Base)) &&
           SUCCEEDED(transform_->SetOutputType(output_id_, type.Get(), 0));
  }

  HRESULT Submit(const uint8_t* pixels) {
    ComPtr<IMFSample> sample;
    ComPtr<IMFMediaBuffer> buffer;
    HRESULT result;
    if (FAILED(result = MFCreateSample(&sample)) ||
        FAILED(result = MFCreateMemoryBuffer(kNv12Size, &buffer))) return result;
    BYTE* destination = nullptr;
    if (FAILED(result = buffer->Lock(&destination, nullptr, nullptr))) return result;
    std::memcpy(destination, pixels, kNv12Size);
    const auto duration = 10'000'000LL / fps_;
    const auto timestamp = index_++ * duration;
    if (FAILED(result = buffer->Unlock()) ||
        FAILED(result = buffer->SetCurrentLength(kNv12Size)) ||
        FAILED(result = sample->AddBuffer(buffer.Get())) ||
        FAILED(result = sample->SetSampleTime(timestamp)) ||
        FAILED(result = sample->SetSampleDuration(duration))) return result;
    return transform_->ProcessInput(input_id_, sample.Get(), 0);
  }

  int Receive(NexDeskEncodedFrame* frame) {
    MFT_OUTPUT_STREAM_INFO info{};
    if (FAILED(transform_->GetOutputStreamInfo(output_id_, &info))) return -1;
    ComPtr<IMFSample> supplied;
    if (!(info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES |
                         MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES))) {
      ComPtr<IMFMediaBuffer> buffer;
      const auto size = (std::max)(info.cbSize, kNv12Size);
      if (size > kMaxEncodedSize || FAILED(MFCreateSample(&supplied)) ||
          FAILED(MFCreateMemoryBuffer(size, &buffer)) ||
          FAILED(supplied->AddBuffer(buffer.Get()))) return -1;
    }
    MFT_OUTPUT_DATA_BUFFER output{output_id_, supplied.Get(), 0, nullptr};
    DWORD status = 0;
    const auto result = transform_->ProcessOutput(0, 1, &output, &status);
    ComPtr<IMFCollection> events;
    events.Attach(output.pEvents);
    ComPtr<IMFSample> produced;
    if (output.pSample && output.pSample != supplied.Get()) produced.Attach(output.pSample);
    if (result == MF_E_TRANSFORM_NEED_MORE_INPUT) return 0;
    if (FAILED(result) || !output.pSample) return -1;
    ComPtr<IMFMediaBuffer> buffer;
    if (FAILED(output.pSample->ConvertToContiguousBuffer(&buffer))) return -1;
    DWORD size = 0;
    if (FAILED(buffer->GetCurrentLength(&size)) || size > kMaxEncodedSize) return -1;
    if (size == 0) return 0;
    bytes_.resize(size);
    BYTE* bytes = nullptr;
    if (FAILED(buffer->Lock(&bytes, nullptr, nullptr))) return -1;
    std::memcpy(bytes_.data(), bytes, size);
    if (FAILED(buffer->Unlock())) return -1;
    UINT32 key = 0;
    output.pSample->GetUINT32(MFSampleExtension_CleanPoint, &key);
    *frame = {bytes_.data(), bytes_.size(), key};
    return 1;
  }

  bool com_ = false;
  bool media_foundation_ = false;
  bool needs_input_ = false;
  DWORD input_id_ = 0, output_id_ = 0;
  UINT32 fps_ = 30;
  LONGLONG index_ = 0;
  ComPtr<IMFActivate> activation_;
  ComPtr<IMFTransform> transform_;
  ComPtr<IMFMediaEventGenerator> events_;
  ComPtr<ICodecAPI> codec_;
  std::vector<uint8_t> bytes_;
};
}  // namespace

extern "C" void* nexdesk_mf_h264_open(uint32_t fps) {
  try {
    auto encoder = std::make_unique<Encoder>();
    if (!encoder->Start(fps)) return nullptr;
    return encoder.release();
  } catch (...) {
    return nullptr;
  }
}

extern "C" int32_t nexdesk_mf_h264_encode(void* handle, const uint8_t* nv12,
                                        size_t size, uint32_t key_frame,
                                        NexDeskEncodedFrame* output) {
  if (!handle) return -1;
  try {
    return static_cast<Encoder*>(handle)->Encode(nv12, size, key_frame != 0, output);
  } catch (...) {
    return -1;
  }
}

extern "C" void nexdesk_mf_h264_close(void* handle) {
  delete static_cast<Encoder*>(handle);
}
