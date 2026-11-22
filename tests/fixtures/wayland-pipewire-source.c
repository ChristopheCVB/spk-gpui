#define _GNU_SOURCE
#include <pipewire/pipewire.h>
#include <spa/param/video/format-utils.h>
#include <spa/pod/builder.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

enum { WIDTH = 640, HEIGHT = 480, STRIDE = WIDTH * 4, SIZE = STRIDE * HEIGHT };

static struct pw_stream *stream;
static struct pw_main_loop *loop;
static struct spa_source *timer;

static void tick(void *unused, uint64_t expirations)
{
    pw_stream_trigger_process(stream);
}

static void state_changed(void *unused, enum pw_stream_state old, enum pw_stream_state state,
                          const char *error)
{
    if (state == PW_STREAM_STATE_ERROR) {
        fprintf(stderr, "PipeWire source: %s\n", error);
        exit(EXIT_FAILURE);
    }
    struct timespec period = { .tv_nsec = 33333333 };
    struct timespec *interval = state == PW_STREAM_STATE_STREAMING ? &period : NULL;
    pw_loop_update_timer(pw_main_loop_get_loop(loop), timer, interval, interval, false);
}

static void format_changed(void *unused, uint32_t id, const struct spa_pod *format)
{
    if (id != SPA_PARAM_Format || !format)
        return;

    uint8_t storage[512];
    struct spa_pod_builder builder = SPA_POD_BUILDER_INIT(storage, sizeof(storage));
    const struct spa_pod *buffers = spa_pod_builder_add_object(
        &builder, SPA_TYPE_OBJECT_ParamBuffers, SPA_PARAM_Buffers,
        SPA_PARAM_BUFFERS_buffers, SPA_POD_Int(8),
        SPA_PARAM_BUFFERS_blocks, SPA_POD_Int(1),
        SPA_PARAM_BUFFERS_size, SPA_POD_Int(SIZE),
        SPA_PARAM_BUFFERS_stride, SPA_POD_Int(STRIDE),
        SPA_PARAM_BUFFERS_dataType, SPA_POD_Int(1 << SPA_DATA_MemFd));
    pw_stream_update_params(stream, &buffers, 1);
}

static void add_buffer(void *unused, struct pw_buffer *buffer)
{
    // WebRTC's ScreenCast consumer maps these file descriptors across processes.
    struct spa_data *data = &buffer->buffer->datas[0];
    int fd = memfd_create("spk-screen", MFD_CLOEXEC);
    if (fd < 0 || ftruncate(fd, SIZE) < 0) {
        perror("allocate PipeWire fixture buffer");
        exit(EXIT_FAILURE);
    }
    void *pixels = mmap(NULL, SIZE, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (pixels == MAP_FAILED) {
        perror("map PipeWire fixture buffer");
        exit(EXIT_FAILURE);
    }
    data->type = SPA_DATA_MemFd;
    data->fd = fd;
    data->flags = SPA_DATA_FLAG_READWRITE;
    data->maxsize = SIZE;
    data->mapoffset = 0;
    data->data = NULL;
    buffer->user_data = pixels;
}

static void remove_buffer(void *unused, struct pw_buffer *buffer)
{
    munmap(buffer->user_data, SIZE);
    close(buffer->buffer->datas[0].fd);
}

static void process(void *unused)
{
    struct pw_buffer *buffer = pw_stream_dequeue_buffer(stream);
    if (!buffer)
        return;

    static uint8_t intensity = 16;
    memset(buffer->user_data, intensity++, SIZE);
    struct spa_chunk *chunk = buffer->buffer->datas[0].chunk;
    chunk->offset = 0;
    chunk->size = SIZE;
    chunk->stride = STRIDE;
    chunk->flags = 0;
    pw_stream_queue_buffer(stream, buffer);
}

int main(int argc, char **argv)
{
    pw_init(&argc, &argv);
    loop = pw_main_loop_new(NULL);
    if (!loop)
        return EXIT_FAILURE;

    struct pw_loop *events = pw_main_loop_get_loop(loop);
    timer = pw_loop_add_timer(events, tick, NULL);
    static const struct pw_stream_events callbacks = {
        PW_VERSION_STREAM_EVENTS,
        .state_changed = state_changed,
        .param_changed = format_changed,
        .add_buffer = add_buffer,
        .remove_buffer = remove_buffer,
        .process = process,
    };
    stream = pw_stream_new_simple(events, "spk-fixture",
                pw_properties_new(PW_KEY_MEDIA_CLASS, "Video/Source", NULL), &callbacks, NULL);
    if (!stream || !timer)
        return EXIT_FAILURE;

    uint8_t storage[512];
    struct spa_pod_builder builder = SPA_POD_BUILDER_INIT(storage, sizeof(storage));
    const struct spa_pod *format = spa_format_video_raw_build(&builder, SPA_PARAM_EnumFormat,
        &SPA_VIDEO_INFO_RAW_INIT(.format = SPA_VIDEO_FORMAT_BGRx,
            .size = SPA_RECTANGLE(WIDTH, HEIGHT), .framerate = SPA_FRACTION(30, 1)));
    if (pw_stream_connect(stream, PW_DIRECTION_OUTPUT, PW_ID_ANY,
            PW_STREAM_FLAG_DRIVER | PW_STREAM_FLAG_ALLOC_BUFFERS, &format, 1) < 0)
        return EXIT_FAILURE;

    pw_main_loop_run(loop);
    pw_stream_destroy(stream);
    pw_main_loop_destroy(loop);
    pw_deinit();
}
