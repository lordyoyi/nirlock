/* pam_nirlock.so — messenger between a PAM host and nirlockd (DESIGN §4).
 *
 * M0 skeleton: argument parsing, PAM_USER / PAM_RHOST checks and the return
 * codes of §4.3 that do not need the socket. The socket exchange (hello /
 * welcome / verify / result, nirlock_wire.c) lands in M4; until then every
 * well-formed call ends in PAM_AUTHINFO_UNAVAIL ("daemon unreachable"),
 * which the lane turns into a deny. Never the "ignore" code (25), never
 * PAM_SUCCESS.
 *
 * Constraints (§4.2): libc + libpam only; no threads, no dlopen, no NSS, no
 * signal handlers, no malloc of our own, no PAM conversation. This module
 * runs inside a fork()ed child of a multithreaded Qt process.
 *
 * SPDX-License-Identifier: MIT
 */

#define _GNU_SOURCE
#include "nirlock_wire.h"

#include <security/pam_ext.h>
#include <security/pam_modules.h>
#include <stdlib.h>
#include <string.h>
#include <syslog.h>

/* The lane relies on this module never returning the "ignore" code (25: an
 * ignored result inside a lane that included system-auth would fail open)
 * and never opening the conversation (the libpam user getter does when
 * PAM_USER is unset; Quickshell bug #977). Make both impossible to type:
 * these two lines are the only places either identifier may appear in this
 * file (DESIGN §10, enforced by `make check-source`). */
#undef PAM_IGNORE
#pragma GCC poison PAM_IGNORE pam_get_user

#define NIRLOCK_DEFAULT_SOCKET "/run/nirlock/sock"
#define NIRLOCK_DEFAULT_TIMEOUT_MS 7000
#define NIRLOCK_MIN_TIMEOUT_MS 1000
#define NIRLOCK_MAX_TIMEOUT_MS 30000
#define NIRLOCK_MAX_USER 32

struct nirlock_opts {
    const char *socket_path;
    long timeout_ms;
    const char *lane;
};

/* Parses "key=value" arguments. Unknown key or bad value → -1 (the caller
 * returns PAM_SERVICE_ERR, §4.1). */
static int parse_opts(pam_handle_t *pamh, int argc, const char **argv, struct nirlock_opts *o)
{
    o->socket_path = NIRLOCK_DEFAULT_SOCKET;
    o->timeout_ms = NIRLOCK_DEFAULT_TIMEOUT_MS;
    o->lane = "lock";
    for (int i = 0; i < argc; i++) {
        const char *a = argv[i];
        if (strncmp(a, "socket=", 7) == 0 && a[7] == '/') {
            o->socket_path = a + 7;
        } else if (strncmp(a, "timeout=", 8) == 0) {
            char *end = NULL;
            long v = strtol(a + 8, &end, 10);
            if (end == a + 8 || *end != '\0' || v < NIRLOCK_MIN_TIMEOUT_MS || v > NIRLOCK_MAX_TIMEOUT_MS) {
                pam_syslog(pamh, LOG_ERR, "bad option %s", a);
                return -1;
            }
            o->timeout_ms = v;
        } else if (strncmp(a, "lane=", 5) == 0) {
            o->lane = a + 5;
        } else {
            pam_syslog(pamh, LOG_ERR, "unknown option %s", a);
            return -1;
        }
    }
    return 0;
}

/* ^[a-z_][a-z0-9_-]{0,31}$ */
static int valid_user(const char *u)
{
    if (u == NULL || *u == '\0')
        return 0;
    size_t n = strnlen(u, NIRLOCK_MAX_USER + 1);
    if (n > NIRLOCK_MAX_USER)
        return 0;
    if (!((u[0] >= 'a' && u[0] <= 'z') || u[0] == '_'))
        return 0;
    for (size_t i = 1; i < n; i++) {
        char c = u[i];
        if (!((c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') || c == '_' || c == '-'))
            return 0;
    }
    return 1;
}

__attribute__((visibility("default")))
int pam_sm_authenticate(pam_handle_t *pamh, int flags, int argc, const char **argv)
{
    (void)flags;
    struct nirlock_opts o;
    if (parse_opts(pamh, argc, argv, &o) != 0)
        return PAM_SERVICE_ERR;
    if (strcmp(o.lane, "lock") != 0) {
        pam_syslog(pamh, LOG_ERR, "lane=%s not supported", o.lane);
        return PAM_SERVICE_ERR;
    }

    /* pam_get_item(PAM_USER), never the prompting getter (§4.2 step 1). */
    const void *item = NULL;
    const char *user = NULL;
    if (pam_get_item(pamh, PAM_USER, &item) == PAM_SUCCESS)
        user = (const char *)item;
    if (!valid_user(user)) {
        pam_syslog(pamh, LOG_NOTICE, "no valid PAM_USER");
        return PAM_USER_UNKNOWN;
    }
    item = NULL;
    if (pam_get_item(pamh, PAM_RHOST, &item) == PAM_SUCCESS && item != NULL && *(const char *)item != '\0') {
        pam_syslog(pamh, LOG_NOTICE, "remote host set; face auth is local only");
        return PAM_AUTH_ERR;
    }

    /* One exchange with the daemon. Everything below this line is the
     * table of §4.3: the module is a messenger, so the only thing it may
     * turn into PAM_SUCCESS is an `accept` that carries our own nonce and
     * user (checked inside nirlock_verify). */
    struct nirlock_result r;
    if (nirlock_verify(o.socket_path, user, o.lane, o.timeout_ms, &r) != 0) {
        pam_syslog(pamh, LOG_ERR, "internal error");
        return PAM_SERVICE_ERR;
    }
    switch (r.outcome) {
    case NL_ACCEPT:
        pam_syslog(pamh, LOG_INFO, "user=%s: accepted by face", user);
        return PAM_SUCCESS;
    case NL_REJECT:
        pam_syslog(pamh, LOG_NOTICE, "user=%s: rejected (%s)", user, r.reason);
        return PAM_AUTH_ERR;
    case NL_LOCKED_OUT:
        pam_syslog(pamh, LOG_NOTICE, "user=%s: locked out (%s)", user, r.reason);
        return PAM_MAXTRIES;
    case NL_UNAVAILABLE:
        pam_syslog(pamh, LOG_INFO, "user=%s: unavailable (%s)", user, r.reason);
        return PAM_AUTHINFO_UNAVAIL;
    case NL_CANCELLED:
        return PAM_ABORT;
    case NL_UNREACHABLE:
        /* No daemon, no welcome, EOF or out of time: indistinguishable to
         * us, and all of them mean "face could not answer", never "no". */
        pam_syslog(pamh, LOG_INFO, "user=%s: daemon unreachable at %s", user, o.socket_path);
        return PAM_AUTHINFO_UNAVAIL;
    case NL_PROTOCOL_ERROR:
    default:
        pam_syslog(pamh, LOG_ERR, "user=%s: protocol error", user);
        return PAM_SERVICE_ERR;
    }
}

__attribute__((visibility("default")))
int pam_sm_setcred(pam_handle_t *pamh, int flags, int argc, const char **argv)
{
    (void)pamh;
    (void)flags;
    (void)argc;
    (void)argv;
    return PAM_SUCCESS;
}
