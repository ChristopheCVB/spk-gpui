#include "modules/desktop_capture/linux/wayland/shared_screencast_stream.h"

using Stream = webrtc::SharedScreenCastStream;
using FrameCallback = void (*)(const void*, const uint8_t*, int32_t, int32_t,
                               int32_t);

extern "C" Stream* spk_pipewire_stream_open(uint32_t node, int32_t remote,
                                           bool cursor) {
  auto stream = Stream::CreateDefault();
  if (!stream->StartScreenCastStream(node, remote, 0, 0, cursor)) {
    return nullptr;
  }
  return stream.release();
}

extern "C" void spk_pipewire_stream_frame(Stream* stream, const void* state,
                                         FrameCallback callback) {
  if (auto frame = stream->CaptureFrame()) {
    callback(state, frame->data(), frame->size().width(), frame->size().height(),
             frame->stride());
  }
}

extern "C" void spk_pipewire_stream_free(Stream* stream) {
  stream->Release();
}
