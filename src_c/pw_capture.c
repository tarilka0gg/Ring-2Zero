/*
 * pw_capture.c  —  PipeWire screencast via xdg-desktop-portal
 *
 * Portal setup:  libdbus-1   (blocking D-Bus calls + signal wait thread)
 * Stream:        libpipewire-0.3 (main-loop + input stream)
 *
 * Public API (called from Rust):
 *   int  pw_capture_start(pw_frame_cb on_frame, void *user_data,
 *                         volatile int *stop_flag,
 *                         char *err_buf, int err_len);
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <stdint.h>
#include <pthread.h>
#include <errno.h>
#include <time.h>

#include <dbus/dbus.h>

#include <pipewire/pipewire.h>
#include <pipewire/stream.h>
#include <spa/param/video/format-utils.h>
#include <spa/pod/builder.h>
#include <spa/utils/result.h>

/* ── Rust callback type ────────────────────────────────────────────────── */

typedef void (*pw_frame_cb)(
    const uint8_t *data,
    uint32_t       width,
    uint32_t       height,
    uint32_t       stride,
    uint32_t       spa_format,   /* SPA_VIDEO_FORMAT_* */
    void          *user_data
);

/* ── Internal context ──────────────────────────────────────────────────── */

struct pw_ctx {
    /* PipeWire */
    struct pw_main_loop *loop;
    struct pw_context   *context;
    struct pw_core      *core;
    struct pw_stream    *stream;
    struct spa_hook      stream_hook;

    /* Config */
    pw_frame_cb       on_frame;
    void             *user_data;
    volatile int     *stop_flag;

    /* Negotiated format */
    uint32_t width, height, stride, spa_fmt;

    int error_code;
};

/* ── SPA pod for format negotiation ────────────────────────────────────── */

static uint32_t build_format_pod(uint8_t *buf, uint32_t buf_size)
{
    struct spa_pod_builder b;
    spa_pod_builder_init(&b, buf, buf_size);

    struct spa_pod_frame f;
    spa_pod_builder_push_object(&b, &f,
        SPA_TYPE_OBJECT_Format, SPA_PARAM_EnumFormat);

    spa_pod_builder_add(&b,
        SPA_FORMAT_mediaType,    SPA_POD_Id(SPA_MEDIA_TYPE_video),
        SPA_FORMAT_mediaSubtype, SPA_POD_Id(SPA_MEDIA_SUBTYPE_raw),
        SPA_FORMAT_VIDEO_format,
            SPA_POD_CHOICE_ENUM_Id(4,
                SPA_VIDEO_FORMAT_BGRA,   /* default */
                SPA_VIDEO_FORMAT_BGRA,
                SPA_VIDEO_FORMAT_RGBA,
                SPA_VIDEO_FORMAT_BGRx),
        0);

    struct spa_pod *pod = spa_pod_builder_pop(&b, &f);
    return (uint32_t)SPA_POD_SIZE(pod);
}

/* ── PipeWire stream events ─────────────────────────────────────────────── */

static void on_param_changed(void *data, uint32_t id, const struct spa_pod *param)
{
    struct pw_ctx *ctx = data;
    if (!param || id != SPA_PARAM_Format) return;

    struct spa_video_info info;
    if (spa_format_video_parse(param, &info) < 0) return;

    if (info.media_subtype == SPA_MEDIA_SUBTYPE_raw) {
        ctx->width   = info.info.raw.size.width;
        ctx->height  = info.info.raw.size.height;
        ctx->spa_fmt = info.info.raw.format;
        /* Provisional stride; on_process() overrides this with the real
         * per-buffer chunk->stride once actual frames start arriving, since
         * the compositor/GPU may pad rows beyond width*4. */
        ctx->stride  = ctx->width * 4;
    }
}

static void on_process(void *data)
{
    struct pw_ctx *ctx = data;

    if (*ctx->stop_flag) {
        pw_main_loop_quit(ctx->loop);
        return;
    }

    struct pw_buffer *pwbuf = pw_stream_dequeue_buffer(ctx->stream);
    if (!pwbuf) return;

    struct spa_buffer *spabuf = pwbuf->buffer;
    struct spa_data   *d      = &spabuf->datas[0];

    if (d->data && d->chunk && d->chunk->size > 0 && ctx->on_frame) {
        /* Use the real per-buffer stride when PipeWire reports one; padded
         * rows (common on GPU-backed buffers) make width*4 wrong. */
        uint32_t stride = d->chunk->stride > 0 ? (uint32_t)d->chunk->stride : ctx->stride;
        ctx->on_frame(
            (const uint8_t *)d->data,
            ctx->width, ctx->height, stride,
            ctx->spa_fmt,
            ctx->user_data
        );
    }

    pw_stream_queue_buffer(ctx->stream, pwbuf);
}

/* KNOWN ISSUE (2026-09-24): in testing under a private/nested portal
 * sandbox, the client stream reliably reaches PW_STREAM_STATE_STREAMING
 * with a correctly negotiated format (confirmed via on_param_changed
 * SPA_PARAM_Format), but on_process below is then never invoked, so no
 * pixel data ever reaches on_frame_cb / the Rust side. Portal negotiation
 * itself (CreateSession/SelectSources/Start, see the fixes above and in
 * wait_for_response/parse_session/parse_start) is confirmed solid and no
 * longer the blocker. Not yet root-caused — worth checking: whether an
 * explicit pw_stream_set_active() is needed here, whether .add_buffer /
 * .remove_buffer stream_events need to be hooked (the server side logs
 * sending 2 "add buffer" events we don't handle), or whether this is
 * specific to the private-bus/nested-compositor sandbox used to test it. */
static const struct pw_stream_events stream_events = {
    PW_VERSION_STREAM_EVENTS,
    .param_changed = on_param_changed,
    .process       = on_process,
};

/* ── Stop-flag watcher thread ───────────────────────────────────────────── */

static void *stop_watcher(void *arg)
{
    struct pw_ctx *ctx = arg;
    while (!*ctx->stop_flag)
        usleep(50000); /* 50 ms */
    pw_main_loop_quit(ctx->loop);
    return NULL;
}

/* ── PipeWire stream run ────────────────────────────────────────────────── */

static int run_pw_stream(int pw_fd, uint32_t node_id, struct pw_ctx *ctx)
{
    pw_init(NULL, NULL);

    ctx->loop = pw_main_loop_new(NULL);
    if (!ctx->loop) return -1;

    ctx->context = pw_context_new(pw_main_loop_get_loop(ctx->loop), NULL, 0);
    if (!ctx->context) return -1;

    /* Connect via the fd given by the portal */
    ctx->core = pw_context_connect_fd(
        ctx->context,
        pw_fd,
        NULL, 0);
    if (!ctx->core) return -1;

    ctx->stream = pw_stream_new(
        ctx->core,
        "ring-2zero-capture",
        pw_properties_new(
            PW_KEY_MEDIA_TYPE,     "Video",
            PW_KEY_MEDIA_CATEGORY, "Capture",
            PW_KEY_MEDIA_ROLE,     "Screen",
            NULL));
    if (!ctx->stream) return -1;

    pw_stream_add_listener(ctx->stream, &ctx->stream_hook,
                           &stream_events, ctx);

    uint8_t pod_buf[1024];
    build_format_pod(pod_buf, sizeof(pod_buf));
    const struct spa_pod *params[1] = {
        (const struct spa_pod *)pod_buf
    };

    int ret = pw_stream_connect(
        ctx->stream,
        PW_DIRECTION_INPUT,
        node_id,
        PW_STREAM_FLAG_AUTOCONNECT | PW_STREAM_FLAG_MAP_BUFFERS,
        params, 1);
    if (ret < 0) return -1;

    /* Watcher thread quits the main loop when stop_flag is set */
    pthread_t watcher_tid;
    pthread_create(&watcher_tid, NULL, stop_watcher, ctx);

    pw_main_loop_run(ctx->loop);

    pthread_join(watcher_tid, NULL);

    pw_stream_destroy(ctx->stream);
    pw_core_disconnect(ctx->core);
    pw_context_destroy(ctx->context);
    pw_main_loop_destroy(ctx->loop);
    pw_deinit();
    return 0;
}

/* ── D-Bus portal helpers ───────────────────────────────────────────────── */

/*
 * Waits for the org.freedesktop.portal.Request.Response signal on
 * request_path, for at most timeout_ms in total, returning early (-1) when
 * *stop becomes non-zero. The caller must already have added a match rule
 * for that signal (see portal_call). Returns the response code negated on
 * failure, or parse_cb's result / 0 on success.
 */
typedef int (*parse_response_cb)(DBusMessageIter *iter, void *data);

static long long now_ms(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long long)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

static int wait_for_response(DBusConnection *bus, const char *request_path,
                              int timeout_ms, volatile int *stop,
                              parse_response_cb parse_cb, void *cb_data)
{
    const long long deadline = now_ms() + timeout_ms;

    while (!*stop && now_ms() < deadline) {
        /* Short slices so a stop request is honoured within ~100 ms */
        if (!dbus_connection_read_write(bus, 100))
            return -1; /* disconnected */

        DBusMessage *msg;
        while ((msg = dbus_connection_pop_message(bus)) != NULL) {
            if (dbus_message_is_signal(msg,
                    "org.freedesktop.portal.Request", "Response") &&
                strcmp(dbus_message_get_path(msg), request_path) == 0) {

                DBusMessageIter iter;
                dbus_message_iter_init(msg, &iter);

                uint32_t response_code = 0;
                dbus_message_iter_get_basic(&iter, &response_code);
                dbus_message_iter_next(&iter);

                int result;
                if (response_code == 0 && parse_cb)
                    result = parse_cb(&iter, cb_data);
                else if (response_code == 0)
                    result = 0;
                else
                    result = -(int)response_code;

                dbus_message_unref(msg);
                return result;
            }
            dbus_message_unref(msg);
        }
    }
    return -1; /* timed out or stopped */
}

/*
 * Builds a unique token (based on pid + counter) and fills in both
 * the handle_token string and the resulting object path for the request.
 */
static void make_token(const char *sender, char *token_out, size_t tok_size,
                       char *path_out,  size_t path_size,
                       const char *suffix)
{
    static unsigned counter = 0;
    unsigned c = __sync_fetch_and_add(&counter, 1);
    snprintf(token_out, tok_size, "ring2zero_%s_%u_%u",
             suffix, (unsigned)getpid(), c);

    /* sender starts with ':', replace '.' and ':' with '_' */
    char sender_clean[64];
    snprintf(sender_clean, sizeof(sender_clean), "%s", sender + 1); /* skip ':' */
    for (char *p = sender_clean; *p; p++)
        if (*p == '.') *p = '_';

    snprintf(path_out, path_size,
             "/org/freedesktop/portal/desktop/request/%s/%s",
             sender_clean, token_out);
}

/* Parse CreateSession response → fill session_path */
struct session_parse_data { char session_path[256]; };
static int parse_session(DBusMessageIter *iter, void *data)
{
    struct session_parse_data *d = data;
    /* iter points at the 'a{sv}' results dict */
    if (dbus_message_iter_get_arg_type(iter) != DBUS_TYPE_ARRAY) return -1;

    DBusMessageIter arr;
    dbus_message_iter_recurse(iter, &arr);
    while (dbus_message_iter_get_arg_type(&arr) == DBUS_TYPE_DICT_ENTRY) {
        DBusMessageIter entry, val;
        dbus_message_iter_recurse(&arr, &entry);
        const char *key = NULL;
        dbus_message_iter_get_basic(&entry, &key);
        dbus_message_iter_next(&entry);
        dbus_message_iter_recurse(&entry, &val);

        if (key && strcmp(key, "session_handle") == 0) {
            /* dbus_message_iter_recurse(&entry, &val) above already
             * unwrapped the variant: val's type here is the variant's
             * actual payload, not DBUS_TYPE_VARIANT itself (checking for
             * DBUS_TYPE_VARIANT again, as this used to, can never be true,
             * so the response was silently discarded on every call, even a
             * successful one). The portal sends session_handle as a plain
             * DBUS_TYPE_STRING here, not DBUS_TYPE_OBJECT_PATH, even
             * though CreateSession's own *argument* uses a real object
             * path (confirmed against a live xdg-desktop-portal-wlr:
             * `variant string "/org/freedesktop/portal/desktop/session/..."`)
             * — both are read identically via get_basic, so accept either. */
            int t = dbus_message_iter_get_arg_type(&val);
            if (t == DBUS_TYPE_STRING || t == DBUS_TYPE_OBJECT_PATH) {
                const char *path = NULL;
                dbus_message_iter_get_basic(&val, &path);
                if (path) strncpy(d->session_path, path, sizeof(d->session_path)-1);
            }
        }
        dbus_message_iter_next(&arr);
    }
    return d->session_path[0] ? 0 : -1;
}

/* Parse Start response → fill node_id and fd */
struct start_parse_data { uint32_t node_id; int pw_fd; };
static int parse_start(DBusMessageIter *iter, void *data)
{
    struct start_parse_data *d = data;
    /* Results dict a{sv} — look for "streams" */
    if (dbus_message_iter_get_arg_type(iter) != DBUS_TYPE_ARRAY) return -1;

    DBusMessageIter arr;
    dbus_message_iter_recurse(iter, &arr);
    while (dbus_message_iter_get_arg_type(&arr) == DBUS_TYPE_DICT_ENTRY) {
        DBusMessageIter entry, val;
        dbus_message_iter_recurse(&arr, &entry);
        const char *key = NULL;
        dbus_message_iter_get_basic(&entry, &key);
        dbus_message_iter_next(&entry);
        dbus_message_iter_recurse(&entry, &val);

        if (key && strcmp(key, "streams") == 0) {
            /* Same unwrap-depth bug as parse_session above: val is already
             * the variant's payload, i.e. directly the a(ua{sv}) array —
             * not a variant to recurse into again. */
            if (dbus_message_iter_get_arg_type(&val) == DBUS_TYPE_ARRAY) {
                DBusMessageIter streams;
                dbus_message_iter_recurse(&val, &streams);

                if (dbus_message_iter_get_arg_type(&streams) == DBUS_TYPE_STRUCT) {
                    DBusMessageIter stream_entry;
                    dbus_message_iter_recurse(&streams, &stream_entry);
                    uint32_t node_id = 0;
                    dbus_message_iter_get_basic(&stream_entry, &node_id);
                    d->node_id = node_id;
                }
            }
        }
        dbus_message_iter_next(&arr);
    }
    return (d->node_id > 0) ? 0 : -1;
}

/* Helper: call a portal method that returns a request handle path */
static int portal_call(DBusConnection *bus,
                       const char *method,
                       void (*add_args)(DBusMessage *msg, void *arg), void *arg,
                       const char *request_path,
                       int timeout_ms, volatile int *stop,
                       parse_response_cb parse_cb, void *cb_data)
{
    /*
     * Subscribe to the Response signal BEFORE making the call. A portal that
     * answers immediately (xdg-desktop-portal-wlr's CreateSession, or any
     * backend configured without a chooser dialog) emits the signal before
     * the method reply is even processed here; subscribing afterwards lost
     * it and the call "failed" after a 30 s wait.
     */
    char match[512];
    snprintf(match, sizeof(match),
        "type='signal',"
        "interface='org.freedesktop.portal.Request',"
        "member='Response',"
        "path='%s'",
        request_path);
    dbus_bus_add_match(bus, match, NULL);

    DBusMessage *msg = dbus_message_new_method_call(
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.ScreenCast",
        method);
    if (!msg) return -1;

    if (add_args) add_args(msg, arg);

    DBusError err;
    dbus_error_init(&err);
    /* The method itself only returns the request handle; the real answer is
     * the Response signal, which may take as long as the user needs. */
    DBusMessage *reply = dbus_connection_send_with_reply_and_block(
        bus, msg, 10000, &err);
    dbus_message_unref(msg);

    int result = -1;
    if (reply && !dbus_error_is_set(&err)) {
        dbus_message_unref(reply);
        result = wait_for_response(bus, request_path, timeout_ms, stop,
                                   parse_cb, cb_data);
    }
    dbus_error_free(&err);
    dbus_bus_remove_match(bus, match, NULL);
    return result;
}

/* ── CreateSession ─────────────────────────────────────────────────────── */

struct create_session_args {
    const char *handle_token;
    const char *session_token;
};

static void add_create_session_args(DBusMessage *msg, void *data)
{
    struct create_session_args *a = data;
    DBusMessageIter iter, arr, entry, val;
    dbus_message_iter_init_append(msg, &iter);
    dbus_message_iter_open_container(&iter, DBUS_TYPE_ARRAY, "{sv}", &arr);

    /* handle_token */
    dbus_message_iter_open_container(&arr, DBUS_TYPE_DICT_ENTRY, NULL, &entry);
    const char *k1 = "handle_token";
    dbus_message_iter_append_basic(&entry, DBUS_TYPE_STRING, &k1);
    dbus_message_iter_open_container(&entry, DBUS_TYPE_VARIANT, "s", &val);
    dbus_message_iter_append_basic(&val, DBUS_TYPE_STRING, &a->handle_token);
    dbus_message_iter_close_container(&entry, &val);
    dbus_message_iter_close_container(&arr, &entry);

    /* session_handle_token */
    dbus_message_iter_open_container(&arr, DBUS_TYPE_DICT_ENTRY, NULL, &entry);
    const char *k2 = "session_handle_token";
    dbus_message_iter_append_basic(&entry, DBUS_TYPE_STRING, &k2);
    dbus_message_iter_open_container(&entry, DBUS_TYPE_VARIANT, "s", &val);
    dbus_message_iter_append_basic(&val, DBUS_TYPE_STRING, &a->session_token);
    dbus_message_iter_close_container(&entry, &val);
    dbus_message_iter_close_container(&arr, &entry);

    dbus_message_iter_close_container(&iter, &arr);
}

/* ── SelectSources ─────────────────────────────────────────────────────── */

struct select_sources_args {
    const char *session_handle;
    const char *handle_token;
};

static void add_select_sources_args(DBusMessage *msg, void *data)
{
    struct select_sources_args *a = data;
    DBusMessageIter iter, arr, entry, val;
    dbus_message_iter_init_append(msg, &iter);

    /* session handle */
    dbus_message_iter_append_basic(&iter, DBUS_TYPE_OBJECT_PATH, &a->session_handle);

    dbus_message_iter_open_container(&iter, DBUS_TYPE_ARRAY, "{sv}", &arr);

    /* handle_token */
    dbus_message_iter_open_container(&arr, DBUS_TYPE_DICT_ENTRY, NULL, &entry);
    const char *k1 = "handle_token";
    dbus_message_iter_append_basic(&entry, DBUS_TYPE_STRING, &k1);
    dbus_message_iter_open_container(&entry, DBUS_TYPE_VARIANT, "s", &val);
    dbus_message_iter_append_basic(&val, DBUS_TYPE_STRING, &a->handle_token);
    dbus_message_iter_close_container(&entry, &val);
    dbus_message_iter_close_container(&arr, &entry);

    /* types = monitor (1) */
    dbus_message_iter_open_container(&arr, DBUS_TYPE_DICT_ENTRY, NULL, &entry);
    const char *k2 = "types";
    dbus_message_iter_append_basic(&entry, DBUS_TYPE_STRING, &k2);
    dbus_message_iter_open_container(&entry, DBUS_TYPE_VARIANT, "u", &val);
    uint32_t types = 1; /* MONITOR */
    dbus_message_iter_append_basic(&val, DBUS_TYPE_UINT32, &types);
    dbus_message_iter_close_container(&entry, &val);
    dbus_message_iter_close_container(&arr, &entry);

    /* cursor_mode = 0 (hidden) */
    dbus_message_iter_open_container(&arr, DBUS_TYPE_DICT_ENTRY, NULL, &entry);
    const char *k3 = "cursor_mode";
    dbus_message_iter_append_basic(&entry, DBUS_TYPE_STRING, &k3);
    dbus_message_iter_open_container(&entry, DBUS_TYPE_VARIANT, "u", &val);
    uint32_t cursor = 2; /* EMBEDDED */
    dbus_message_iter_append_basic(&val, DBUS_TYPE_UINT32, &cursor);
    dbus_message_iter_close_container(&entry, &val);
    dbus_message_iter_close_container(&arr, &entry);

    dbus_message_iter_close_container(&iter, &arr);
}

/* ── Start ─────────────────────────────────────────────────────────────── */

struct start_args {
    const char *session_handle;
    const char *handle_token;
};

static void add_start_args(DBusMessage *msg, void *data)
{
    struct start_args *a = data;
    DBusMessageIter iter, arr, entry, val;
    dbus_message_iter_init_append(msg, &iter);

    dbus_message_iter_append_basic(&iter, DBUS_TYPE_OBJECT_PATH, &a->session_handle);
    const char *parent_window = "";
    dbus_message_iter_append_basic(&iter, DBUS_TYPE_STRING, &parent_window);

    dbus_message_iter_open_container(&iter, DBUS_TYPE_ARRAY, "{sv}", &arr);
    dbus_message_iter_open_container(&arr, DBUS_TYPE_DICT_ENTRY, NULL, &entry);
    const char *k1 = "handle_token";
    dbus_message_iter_append_basic(&entry, DBUS_TYPE_STRING, &k1);
    dbus_message_iter_open_container(&entry, DBUS_TYPE_VARIANT, "s", &val);
    dbus_message_iter_append_basic(&val, DBUS_TYPE_STRING, &a->handle_token);
    dbus_message_iter_close_container(&entry, &val);
    dbus_message_iter_close_container(&arr, &entry);
    dbus_message_iter_close_container(&iter, &arr);
}

/* ── OpenPipeWireRemote ────────────────────────────────────────────────── */

static int open_pw_remote(DBusConnection *bus, const char *session_handle)
{
    DBusMessage *msg = dbus_message_new_method_call(
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.ScreenCast",
        "OpenPipeWireRemote");
    if (!msg) return -1;

    DBusMessageIter iter, arr;
    dbus_message_iter_init_append(msg, &iter);
    dbus_message_iter_append_basic(&iter, DBUS_TYPE_OBJECT_PATH, &session_handle);
    dbus_message_iter_open_container(&iter, DBUS_TYPE_ARRAY, "{sv}", &arr);
    dbus_message_iter_close_container(&iter, &arr);

    DBusError err;
    dbus_error_init(&err);
    DBusMessage *reply = dbus_connection_send_with_reply_and_block(
        bus, msg, 10000, &err);
    dbus_message_unref(msg);

    if (!reply || dbus_error_is_set(&err)) {
        dbus_error_free(&err);
        return -1;
    }

    int fd = -1;
    DBusMessageIter reply_iter;
    dbus_message_iter_init(reply, &reply_iter);
    if (dbus_message_iter_get_arg_type(&reply_iter) == DBUS_TYPE_UNIX_FD) {
        dbus_message_iter_get_basic(&reply_iter, &fd);
        fd = dup(fd); /* own copy */
    }
    dbus_message_unref(reply);
    return fd;
}

/*
 * Fire-and-forget close of a portal Session object. Every CreateSession
 * that succeeds must be matched by exactly one of these, on every exit path
 * (error or clean stop) — otherwise a session that's never explicitly
 * closed lingers in the portal backend. Observed in testing: enough
 * abandoned sessions (from repeated capture retries after a failure, with
 * no backoff above it) made xdg-desktop-portal-wlr's own sd_bus_add_object_
 * vtable() start failing with EAGAIN, breaking ScreenCast for the rest of
 * that backend's lifetime, not just for this client.
 */
static void close_portal_session(DBusConnection *bus, const char *session_path)
{
    if (!bus || !session_path || !*session_path) return;
    DBusMessage *msg = dbus_message_new_method_call(
        "org.freedesktop.portal.Desktop",
        session_path,
        "org.freedesktop.impl.portal.Session",
        "Close");
    if (!msg) return;
    /* No reply needed, and no response signal to wait for. */
    dbus_message_set_no_reply(msg, TRUE);
    dbus_connection_send(bus, msg, NULL);
    dbus_connection_flush(bus);
    dbus_message_unref(msg);
}

/* ── Public entry point ─────────────────────────────────────────────────── */

int pw_capture_start(
    pw_frame_cb   on_frame,
    void         *user_data,
    volatile int *stop_flag,
    char         *err_buf,
    int           err_len)
{
    /* ── D-Bus portal setup ─────────────────────────────────────────── */
    DBusError dbus_err;
    dbus_error_init(&dbus_err);
    DBusConnection *bus = dbus_bus_get(DBUS_BUS_SESSION, &dbus_err);
    if (!bus || dbus_error_is_set(&dbus_err)) {
        snprintf(err_buf, err_len, "dbus_bus_get: %s", dbus_err.message);
        dbus_error_free(&dbus_err);
        return -1;
    }
    dbus_connection_set_exit_on_disconnect(bus, FALSE);

    const char *sender = dbus_bus_get_unique_name(bus);

    /* 1. CreateSession */
    char htok1[64], hpath1[256];
    char stok[64];
    /* Must be unique per CALL, not just per process: this function is
     * retried from the same long-lived process on every capture failure
     * (backend crash, portal timeout, user cancel), and a session token
     * that only encoded the pid was identical on every retry. The portal
     * forwards CreateSession's caller-supplied session path unchanged to
     * the backend, so a second attempt while the first session's object
     * was still registered (Close() not yet processed, or simply racing
     * the retry) had the wlr backend try to add a D-Bus vtable at a path
     * it already had one on — observed as CreateSession failing forever
     * after the first retry, with xdg-desktop-portal-wlr logging
     * "sd_bus_add_object_vtable failed: Unknown error -11". */
    snprintf(stok, sizeof(stok), "ring2zero_sess_%u_%lld",
             (unsigned)getpid(), now_ms());
    make_token(sender, htok1, sizeof(htok1), hpath1, sizeof(hpath1), "cs");

    struct session_parse_data sess_data = {0};
    struct create_session_args cs_args = { htok1, stok };
    if (portal_call(bus, "CreateSession",
                    add_create_session_args, &cs_args,
                    hpath1, 30000, stop_flag, parse_session, &sess_data) < 0) {
        snprintf(err_buf, err_len, "CreateSession failed");
        dbus_connection_unref(bus);
        return -1;
    }

    /* 2. SelectSources */
    char htok2[64], hpath2[256];
    make_token(sender, htok2, sizeof(htok2), hpath2, sizeof(hpath2), "ss");
    struct select_sources_args ss_args = { sess_data.session_path, htok2 };
    if (portal_call(bus, "SelectSources",
                    add_select_sources_args, &ss_args,
                    hpath2, 30000, stop_flag, NULL, NULL) < 0) {
        snprintf(err_buf, err_len, "SelectSources failed");
        close_portal_session(bus, sess_data.session_path);
        dbus_connection_unref(bus);
        return -1;
    }

    /* 3. Start  (user sees approval dialog here) */
    char htok3[64], hpath3[256];
    make_token(sender, htok3, sizeof(htok3), hpath3, sizeof(hpath3), "st");
    struct start_parse_data start_data = {0};
    struct start_args st_args = { sess_data.session_path, htok3 };
    if (portal_call(bus, "Start",
                    add_start_args, &st_args,
                    hpath3, 120000 /* 2 min for user */, stop_flag,
                    parse_start, &start_data) < 0) {
        snprintf(err_buf, err_len, "Start failed (user cancelled?)");
        close_portal_session(bus, sess_data.session_path);
        dbus_connection_unref(bus);
        return -1;
    }

    /* 4. OpenPipeWireRemote */
    int pw_fd = open_pw_remote(bus, sess_data.session_path);
    if (pw_fd < 0) {
        snprintf(err_buf, err_len, "OpenPipeWireRemote failed");
        close_portal_session(bus, sess_data.session_path);
        dbus_connection_unref(bus);
        return -1;
    }

    /* ── PipeWire stream ───────────────────────────────────────────── */
    struct pw_ctx ctx = {
        .on_frame  = on_frame,
        .user_data = user_data,
        .stop_flag = stop_flag,
    };

    int ret = run_pw_stream(pw_fd, start_data.node_id, &ctx);
    close(pw_fd);

    /* Runs on every exit from here on, including a clean stop: the session
     * this whole call negotiated must not outlive the capture. */
    close_portal_session(bus, sess_data.session_path);
    dbus_connection_unref(bus);

    if (ret < 0) {
        snprintf(err_buf, err_len, "PipeWire stream error");
        return -1;
    }
    return 0;
}
