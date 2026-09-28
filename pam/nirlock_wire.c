/* See nirlock_wire.h. SPDX-License-Identifier: MIT */

#define _GNU_SOURCE
#include "nirlock_wire.h"

#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdio.h>
#include <string.h>
#include <sys/random.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <time.h>
#include <unistd.h>

#define NL_MAX_LINE 8192
#define NL_CONNECT_MS 500
#define NL_WELCOME_MS 2500
#define NL_BUDGET_MIN 1000
#define NL_BUDGET_MAX 4000
/* Below this there is no point starting: the daemon needs ~1000 ms of
 * budget and 300 ms of slack to answer inside it (PROTOCOL §8). */
#define NL_MIN_REMAINING 1300
#define NL_PROTO 1

static long now_ms(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

/* 32 lowercase hex characters from the kernel. A predictable nonce would
 * let a same-uid process replay a stolen `accept`. */
static int make_nonce(char out[33])
{
    unsigned char raw[16];
    size_t got = 0;
    while (got < sizeof raw) {
        ssize_t n = getrandom(raw + got, sizeof raw - got, 0);
        if (n < 0) {
            if (errno == EINTR)
                continue;
            return -1;
        }
        got += (size_t)n;
    }
    static const char hex[] = "0123456789abcdef";
    for (size_t i = 0; i < sizeof raw; i++) {
        out[2 * i] = hex[raw[i] >> 4];
        out[2 * i + 1] = hex[raw[i] & 0xf];
    }
    out[32] = '\0';
    return 0;
}

/* connect(2) bounded by NL_CONNECT_MS. Returns the fd or -1. */
static int connect_deadline(const char *path)
{
    struct sockaddr_un sa;
    size_t n = strlen(path);
    if (n >= sizeof sa.sun_path)
        return -1;
    memset(&sa, 0, sizeof sa);
    sa.sun_family = AF_UNIX;
    memcpy(sa.sun_path, path, n + 1);

    int fd = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC | SOCK_NONBLOCK, 0);
    if (fd < 0)
        return -1;
    if (connect(fd, (struct sockaddr *)&sa, sizeof sa) == 0)
        goto ok;
    if (errno != EINPROGRESS) {
        close(fd);
        return -1;
    }
    {
        struct pollfd p = {.fd = fd, .events = POLLOUT, .revents = 0};
        int r;
        do {
            r = poll(&p, 1, NL_CONNECT_MS);
        } while (r < 0 && errno == EINTR);
        int err = 0;
        socklen_t len = sizeof err;
        if (r != 1 || getsockopt(fd, SOL_SOCKET, SO_ERROR, &err, &len) != 0 || err != 0) {
            close(fd);
            return -1;
        }
    }
ok:
    /* Back to blocking; every read is bounded by its own poll. */
    fcntl(fd, F_SETFL, fcntl(fd, F_GETFL, 0) & ~O_NONBLOCK);
    return fd;
}

static int write_all(int fd, const char *buf, size_t n)
{
    size_t off = 0;
    while (off < n) {
        /* MSG_NOSIGNAL: a daemon that closed early must not kill the PAM
         * host with SIGPIPE — we are inside somebody else's process. */
        ssize_t w = send(fd, buf + off, n - off, MSG_NOSIGNAL);
        if (w < 0) {
            if (errno == EINTR)
                continue;
            return -1;
        }
        off += (size_t)w;
    }
    return 0;
}

/* Reads one '\n'-terminated line, bounded by `deadline`. Returns its length
 * without the newline, or -1 on timeout, EOF, overlong line or error. */
static ssize_t read_line(int fd, char *buf, size_t cap, long deadline)
{
    size_t used = 0;
    for (;;) {
        long left = deadline - now_ms();
        if (left <= 0)
            return -1;
        struct pollfd p = {.fd = fd, .events = POLLIN, .revents = 0};
        int r = poll(&p, 1, (int)left);
        if (r < 0) {
            if (errno == EINTR)
                continue;
            return -1;
        }
        if (r == 0)
            return -1;
        ssize_t n = read(fd, buf + used, cap - 1 - used);
        if (n < 0) {
            if (errno == EINTR)
                continue;
            return -1;
        }
        if (n == 0)
            return -1; /* EOF before a complete line */
        used += (size_t)n;
        buf[used] = '\0';
        char *nl = memchr(buf, '\n', used);
        if (nl != NULL) {
            *nl = '\0';
            return nl - buf;
        }
        if (used >= cap - 1)
            return -1; /* overlong: refuse without parsing */
    }
}

/* Copies the value of `"key":"..."` into `dst`. Returns 0 on success. The
 * daemon's `result` for a pam client has a fixed shape (PROTOCOL §6.2), so
 * this deliberately understands nothing more than flat string fields. */
static int json_str(const char *line, const char *key, char *dst, size_t cap)
{
    char pat[32];
    int k = snprintf(pat, sizeof pat, "\"%s\":\"", key);
    if (k < 0 || (size_t)k >= sizeof pat)
        return -1;
    const char *p = strstr(line, pat);
    if (p == NULL)
        return -1;
    p += k;
    const char *end = strchr(p, '"');
    if (end == NULL || (size_t)(end - p) >= cap)
        return -1;
    memcpy(dst, p, (size_t)(end - p));
    dst[end - p] = '\0';
    return 0;
}

int nirlock_verify(const char *socket_path, const char *user, const char *lane,
                   long deadline_ms, struct nirlock_result *out)
{
    if (out == NULL)
        return -1;
    out->outcome = NL_UNREACHABLE;
    out->reason[0] = '\0';

    const long deadline = now_ms() + deadline_ms;
    int fd = connect_deadline(socket_path);
    if (fd < 0)
        return 0; /* no daemon: unreachable, the lane denies */

    char line[NL_MAX_LINE];
    int rc = 0;
    int n = snprintf(line, sizeof line,
                     "{\"t\":\"hello\",\"v\":%d,\"client\":\"pam\",\"ver\":\"%s\"}\n", NL_PROTO,
                     NIRLOCK_VERSION);
    if (n < 0 || (size_t)n >= sizeof line || write_all(fd, line, (size_t)n) != 0)
        goto done;

    {
        long wdl = now_ms() + NL_WELCOME_MS;
        if (wdl > deadline)
            wdl = deadline;
        if (read_line(fd, line, sizeof line, wdl) < 0)
            goto done; /* no welcome: unreachable */
    }

    {
        long left = deadline - now_ms();
        if (left < NL_MIN_REMAINING)
            goto done; /* too late to start; do not make the host wait */
        long budget = left - 300;
        if (budget < NL_BUDGET_MIN)
            budget = NL_BUDGET_MIN;
        if (budget > NL_BUDGET_MAX)
            budget = NL_BUDGET_MAX;

        char nonce[33];
        if (make_nonce(nonce) != 0) {
            out->outcome = NL_PROTOCOL_ERROR;
            goto done;
        }
        n = snprintf(line, sizeof line,
                     "{\"t\":\"verify\",\"v\":%d,\"nonce\":\"%s\",\"user\":\"%s\",\"lane\":\"%s\","
                     "\"service\":\"\",\"tty\":\"\",\"rhost\":\"\",\"budget_ms\":%ld}\n",
                     NL_PROTO, nonce, user, lane, budget);
        if (n < 0 || (size_t)n >= sizeof line || write_all(fd, line, (size_t)n) != 0)
            goto done;

        if (read_line(fd, line, sizeof line, deadline) < 0)
            goto done; /* EOF or timeout while a result was owed */

        char got_nonce[64] = {0}, got_user[64] = {0}, outcome[32] = {0};
        if (json_str(line, "nonce", got_nonce, sizeof got_nonce) != 0 ||
            json_str(line, "user", got_user, sizeof got_user) != 0 ||
            json_str(line, "outcome", outcome, sizeof outcome) != 0) {
            out->outcome = NL_PROTOCOL_ERROR;
            goto done;
        }
        /* An answer that is not about this request is never an accept:
         * both fields must match what we asked. */
        if (strcmp(got_nonce, nonce) != 0 || strcmp(got_user, user) != 0) {
            out->outcome = NL_PROTOCOL_ERROR;
            goto done;
        }
        (void)json_str(line, "reason", out->reason, sizeof out->reason);

        if (strcmp(outcome, "accept") == 0)
            out->outcome = NL_ACCEPT;
        else if (strcmp(outcome, "reject") == 0)
            out->outcome = NL_REJECT;
        else if (strcmp(outcome, "locked_out") == 0)
            out->outcome = NL_LOCKED_OUT;
        else if (strcmp(outcome, "unavailable") == 0)
            out->outcome = NL_UNAVAILABLE;
        else if (strcmp(outcome, "cancelled") == 0)
            out->outcome = NL_CANCELLED;
        else
            out->outcome = NL_PROTOCOL_ERROR;
    }

done:
    close(fd);
    return rc;
}
