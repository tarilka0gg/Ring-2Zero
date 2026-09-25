/*
 * pw_audio_capture.c  —  Capture the default sink's monitor (i.e. "what you
 * hear") directly from PipeWire, no xdg-desktop-portal involved.
 *
 * Unlike screen capture, recording a monitor port needs no permission
 * prompt — any client in the same session can already do it (that's how
 * pavucontrol/pw-record/OBS's "desktop audio" source all work), so this
 * connects straight to the local PipeWire session instead of going through
 * pw_capture.c's D-Bus portal dance.
 *
 * PW_KEY_STREAM_CAPTURE_SINK=true is the actual trick: it tells the session
 * manager (WirePlumber) to autoconnect this input stream to the *monitor*
 * port of the default sink rather than a source, which is otherwise
 * indistinguishable from an ordinary capture stream from PipeWire's point
 * of view.
 *
 * Public API (called from Rust):
 *   int  pw_audio_capture_start(pw_audio_cb on_audio, void *user_data,
 *                                volatile int *stop_flag,
 *                                char *err_buf, int err_len);
 */

#include <stdint.h>
#include <stdio.h>
#include <pthread.h>
#include <unistd.h>

#include <pipewire/pipewire.h>
#include <pipewire/stream.h>
#include <spa/param/audio/format-utils.h>
#include <spa/param/buffers.h>
#include <spa/pod/builder.h>

#define CAPTURE_RATE     48000
#define CAPTURE_CHANNELS 2

typedef void (*pw_audio_cb)(
    const uint8_t *data,
    uint32_t       n_samples, /* interleaved f32 samples: frames * channels */
    uint32_t       channels,
    uint32_t       rate,
    void          *user_data
);

struct pw_audio_ctx {
    struct pw_main_loop *loop;
    struct pw_context   *context;
    struct pw_core      *core;
    struct pw_stream    *stream;
    struct spa_hook      stream_hook;

    pw_audio_cb       on_audio;
    void             *user_data;
    volatile int     *stop_flag;

    uint32_t channels, rate;
    int error_code;
};

static uint32_t build_audio_format_pod(uint8_t *buf, uint32_t buf_size)
{
    struct spa_pod_builder b;
    spa_pod_builder_init(&b, buf, buf_size);

    struct spa_pod_frame f;
    spa_pod_builder_push_object(&b, &f,
        SPA_TYPE_OBJECT_Format, SPA_PARAM_EnumFormat);

    spa_pod_builder_add(&b,
        SPA_FORMAT_mediaType,    SPA_POD_Id(SPA_MEDIA_TYPE_audio),
        SPA_FORMAT_mediaSubtype, SPA_POD_Id(SPA_MEDIA_SUBTYPE_raw),
        SPA_FORMAT_AUDIO_format, SPA_POD_Id(SPA_AUDIO_FORMAT_F32),
        SPA_FORMAT_AUDIO_rate,   SPA_POD_Int(CAPTURE_RATE),
        SPA_FORMAT_AUDIO_channels, SPA_POD_Int(CAPTURE_CHANNELS),
        0);

    struct spa_pod *pod = spa_pod_builder_pop(&b, &f);
    return (uint32_t)SPA_POD_SIZE(pod);
}

static void on_param_changed(void *data, uint32_t id, const struct spa_pod *param)
{
    struct pw_audio_ctx *ctx = data;
    if (!param || id != SPA_PARAM_Format) return;

    struct spa_audio_info info = { 0 };
    if (spa_format_parse(param, &info.media_type, &info.media_subtype) < 0) return;
    if (info.media_type != SPA_MEDIA_TYPE_audio || info.media_subtype != SPA_MEDIA_SUBTYPE_raw)
        return;
    if (spa_format_audio_raw_parse(param, &info.info.raw) < 0) return;

    ctx->channels = info.info.raw.channels;
    ctx->rate     = info.info.raw.rate;

    /* Same requirement as the video path (see pw_capture.c): accepting
     * SPA_PARAM_Format alone doesn't finish negotiation — buffer params
     * must be sent back before PipeWire will ever call .process(). */
    uint32_t stride = ctx->channels * (uint32_t)sizeof(float);
    uint8_t buf[1024];
    struct spa_pod_builder b;
    spa_pod_builder_init(&b, buf, sizeof(buf));
    const struct spa_pod *params[1];
    params[0] = spa_pod_builder_add_object(&b,
        SPA_TYPE_OBJECT_ParamBuffers, SPA_PARAM_Buffers,
        SPA_PARAM_BUFFERS_buffers, SPA_POD_CHOICE_RANGE_Int(8, 2, 32),
        SPA_PARAM_BUFFERS_blocks,  SPA_POD_Int(1),
        SPA_PARAM_BUFFERS_size,    SPA_POD_CHOICE_RANGE_Int(stride * 1024, stride, stride * 1024 * 32),
        SPA_PARAM_BUFFERS_stride,  SPA_POD_Int(stride));
    pw_stream_update_params(ctx->stream, params, 1);
}

static void on_process(void *data)
{
    struct pw_audio_ctx *ctx = data;

    if (*ctx->stop_flag) {
        pw_main_loop_quit(ctx->loop);
        return;
    }

    struct pw_buffer *pwbuf = pw_stream_dequeue_buffer(ctx->stream);
    if (!pwbuf) return;

    struct spa_buffer *spabuf = pwbuf->buffer;
    struct spa_data   *d      = &spabuf->datas[0];

    if (d->data && d->chunk && d->chunk->size > 0 && d->maxsize > 0
        && ctx->on_audio && ctx->channels > 0) {
        /* Same offset/bounds discipline as pw_capture.c's on_process: never
         * trust chunk->size beyond what maxsize actually backs. */
        uint32_t offset = d->chunk->offset % d->maxsize;
        uint32_t cap     = d->maxsize - offset;
        uint32_t size    = (uint32_t)d->chunk->size;
        if (size > cap) size = cap;

        uint32_t frame_bytes = ctx->channels * (uint32_t)sizeof(float);
        uint32_t n_samples   = frame_bytes > 0 ? (size / frame_bytes) * ctx->channels : 0;

        if (n_samples > 0) {
            ctx->on_audio(
                (const uint8_t *)d->data + offset,
                n_samples, ctx->channels, ctx->rate,
                ctx->user_data
            );
        }
    }

    pw_stream_queue_buffer(ctx->stream, pwbuf);
}

static const struct pw_stream_events stream_events = {
    PW_VERSION_STREAM_EVENTS,
    .param_changed = on_param_changed,
    .process       = on_process,
};

static void *stop_watcher(void *arg)
{
    struct pw_audio_ctx *ctx = arg;
    while (!*ctx->stop_flag)
        usleep(50000);
    pw_main_loop_quit(ctx->loop);
    return NULL;
}

int pw_audio_capture_start(
    pw_audio_cb   on_audio,
    void         *user_data,
    volatile int *stop_flag,
    char         *err_buf,
    int           err_len)
{
    struct pw_audio_ctx ctx = {
        .on_audio  = on_audio,
        .user_data = user_data,
        .stop_flag = stop_flag,
    };

    pw_init(NULL, NULL);

    ctx.loop = pw_main_loop_new(NULL);
    if (!ctx.loop) {
        snprintf(err_buf, err_len, "pw_main_loop_new failed");
        pw_deinit();
        return -1;
    }

    ctx.context = pw_context_new(pw_main_loop_get_loop(ctx.loop), NULL, 0);
    if (!ctx.context) {
        snprintf(err_buf, err_len, "pw_context_new failed");
        pw_main_loop_destroy(ctx.loop);
        pw_deinit();
        return -1;
    }

    ctx.core = pw_context_connect(ctx.context, NULL, 0);
    if (!ctx.core) {
        snprintf(err_buf, err_len, "pw_context_connect failed (no PipeWire session?)");
        pw_context_destroy(ctx.context);
        pw_main_loop_destroy(ctx.loop);
        pw_deinit();
        return -1;
    }

    ctx.stream = pw_stream_new(
        ctx.core,
        "ring-2zero-audio",
        pw_properties_new(
            PW_KEY_MEDIA_TYPE,          "Audio",
            PW_KEY_MEDIA_CATEGORY,      "Capture",
            PW_KEY_MEDIA_ROLE,          "Music",
            PW_KEY_STREAM_CAPTURE_SINK, "true", /* record the default sink's monitor */
            NULL));
    if (!ctx.stream) {
        snprintf(err_buf, err_len, "pw_stream_new failed");
        pw_core_disconnect(ctx.core);
        pw_context_destroy(ctx.context);
        pw_main_loop_destroy(ctx.loop);
        pw_deinit();
        return -1;
    }

    pw_stream_add_listener(ctx.stream, &ctx.stream_hook, &stream_events, &ctx);

    uint8_t pod_buf[1024];
    build_audio_format_pod(pod_buf, sizeof(pod_buf));
    const struct spa_pod *params[1] = { (const struct spa_pod *)pod_buf };

    int ret = pw_stream_connect(
        ctx.stream,
        PW_DIRECTION_INPUT,
        PW_ID_ANY,
        PW_STREAM_FLAG_AUTOCONNECT | PW_STREAM_FLAG_MAP_BUFFERS,
        params, 1);
    if (ret < 0) {
        snprintf(err_buf, err_len, "pw_stream_connect failed: %d", ret);
        pw_stream_destroy(ctx.stream);
        pw_core_disconnect(ctx.core);
        pw_context_destroy(ctx.context);
        pw_main_loop_destroy(ctx.loop);
        pw_deinit();
        return -1;
    }

    pthread_t watcher_tid;
    pthread_create(&watcher_tid, NULL, stop_watcher, &ctx);

    pw_main_loop_run(ctx.loop);

    pthread_join(watcher_tid, NULL);

    pw_stream_destroy(ctx.stream);
    pw_core_disconnect(ctx.core);
    pw_context_destroy(ctx.context);
    pw_main_loop_destroy(ctx.loop);
    pw_deinit();
    return 0;
}
