/* Minimal client of the nirlock socket protocol for the PAM module.
 *
 * Constraints inherited from pam_nirlock.c (DESIGN §4.2): libc only, no
 * threads, no dlopen, no NSS, no signal handlers, no heap of our own, no
 * PAM conversation. Everything here works on stack buffers.
 *
 * SPDX-License-Identifier: MIT
 */
#ifndef NIRLOCK_WIRE_H
#define NIRLOCK_WIRE_H

#include <stddef.h>

enum nirlock_outcome {
    NL_ACCEPT = 0,     /* the daemon recognised the user                  */
    NL_REJECT,         /* a decision was made and it was "no"             */
    NL_LOCKED_OUT,     /* policy refuses to try (PAM_MAXTRIES)            */
    NL_UNAVAILABLE,    /* could not try: camera busy, stale, not enrolled */
    NL_CANCELLED,      /* the request was cancelled                       */
    NL_UNREACHABLE,    /* no daemon: no socket, refused, EOF, timeout     */
    NL_PROTOCOL_ERROR  /* malformed, mismatched nonce/user, bad version   */
};

struct nirlock_result {
    enum nirlock_outcome outcome;
    char reason[32]; /* the daemon's `reason`, for the journal; may be "" */
};

/* Runs one hello/welcome/verify/result exchange.
 *
 * `deadline_ms` is the whole budget from the caller's point of view; the
 * per-step timeouts of PROTOCOL §8 are derived from it, and the `budget_ms`
 * sent to the daemon is `remaining - 300` clamped to [1000, 4000]. If less
 * than 1300 ms remain once `welcome` has arrived, no `verify` is sent and
 * NL_UNREACHABLE is returned: answering late is worse than not answering.
 *
 * Never blocks past `deadline_ms`. Returns 0 on a completed exchange (check
 * `out->outcome`) and -1 if `out` could not be filled at all.
 */
int nirlock_verify(const char *socket_path, const char *user, const char *lane,
                   long deadline_ms, struct nirlock_result *out);

#endif
