/* Lane harness (extends research/prototypes/pamtest/t.c, no root).
 *
 * Writes a temporary PAM confdir with (a) the PACKAGED nirlock-lock lane
 * read from pam.d/nirlock-lock with only the `pam_nirlock.so` token replaced
 * by the freshly built .so, (b) `nirlock-direct`, a lane with the module
 * alone as `required` so its RAW return codes of DESIGN §4.3 are observed
 * (9 / 10 / 3 / 7) rather than inferred through pam_deny, and (c) `other`
 * plus the fail-open canaries; then runs pam_start_confdir +
 * pam_authenticate through the real libpam and prints the code each case
 * yields. The conversation aborts the process if it is ever invoked: this
 * module must never open one (§4.2).
 *
 * Usage: lane_test <path/to/pam_nirlock.so> <path/to/pam.d/nirlock-lock>
 * Exit 0 when every measured code matches the expectation table. */
#include <errno.h>
#include <security/pam_appl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static int conv_abort(int n, const struct pam_message **m, struct pam_response **r, void *p)
{
    (void)n;
    (void)m;
    (void)r;
    (void)p;
    fprintf(stderr, "FAIL: PAM conversation invoked; pam_nirlock must never do that\n");
    abort();
}

static void write_file(const char *path, const char *text)
{
    FILE *f = fopen(path, "w");
    if (!f) {
        perror(path);
        exit(2);
    }
    fputs(text, f);
    fclose(f);
}

/* Copies the packaged lane to `dst`, replacing every whitespace-delimited
 * `pam_nirlock.so` token with `module` (an absolute path). Any other edit
 * would defeat the purpose: the test must exercise the shipped text. */
/* A verbatim copy. `other` has no module token to substitute, so
 * write_lane_from_packaged (which requires exactly one) cannot be used. */
static void copy_file(const char *src, const char *dst)
{
    FILE *in = fopen(src, "r");
    if (!in) {
        perror(src);
        exit(2);
    }
    FILE *out = fopen(dst, "w");
    if (!out) {
        perror(dst);
        exit(2);
    }
    int c;
    while ((c = fgetc(in)) != EOF)
        fputc(c, out);
    fclose(in);
    fclose(out);
}

/* The generated lanes point at a socket that cannot exist, so the "daemon
 * unreachable" cases are hermetic. Without this the harness connected to
 * /run/nirlock/sock, the real daemon, and on the author's machine — the only
 * one with a camera — it performed REAL face verifications: 20 of them in one
 * `make test`, every one a match, so three tests that expect "unreachable"
 * got a genuine success and failed. A test suite that authenticates you is
 * not a test suite. */
#define NO_DAEMON_SOCKET "socket=/nonexistent/nirlock-test-no-daemon.sock"

/* The one case that must use the packaged lane verbatim cannot be redirected
 * that way, so it is skipped when a daemon is actually listening. */
static int daemon_is_live(void)
{
    return access("/run/nirlock/sock", F_OK) == 0;
}

static void write_lane_from_packaged(const char *src, const char *dst, const char *module)
{
    FILE *in = fopen(src, "r");
    if (!in) {
        perror(src);
        exit(2);
    }
    FILE *out = fopen(dst, "w");
    if (!out) {
        perror(dst);
        exit(2);
    }
    char line[1024];
    int substituted = 0;
    while (fgets(line, sizeof line, in)) {
        const char *tok = "pam_nirlock.so";
        const char *p = line;
        const char *hit;
        while ((hit = strstr(p, tok)) != NULL) {
            int at_start = hit == line || hit[-1] == ' ' || hit[-1] == '\t';
            const char *end = hit + strlen(tok);
            int at_end = *end == '\0' || *end == ' ' || *end == '\t' || *end == '\n';
            fwrite(p, 1, (size_t)(hit - p), out);
            if (at_start && at_end) {
                fputs(module, out);
                substituted++;
            } else {
                fputs(tok, out);
            }
            p = end;
        }
        fputs(p, out);
    }
    fclose(in);
    fclose(out);
    if (substituted != 1) {
        fprintf(stderr, "FAIL: expected exactly one pam_nirlock.so token in %s, found %d\n", src, substituted);
        exit(2);
    }
}

static int run(const char *confdir, const char *service, const char *user, const char *rhost)
{
    struct pam_conv cv = {conv_abort, NULL};
    pam_handle_t *h = NULL;
    int rc = pam_start_confdir(service, user, &cv, confdir, &h);
    if (rc != PAM_SUCCESS) {
        printf("  pam_start_confdir(%s) rc=%d (%s)\n", service, rc, pam_strerror(NULL, rc));
        return -rc;
    }
    if (rhost)
        pam_set_item(h, PAM_RHOST, rhost);
    rc = pam_authenticate(h, 0);
    pam_end(h, rc);
    return rc;
}

struct tcase {
    const char *name;
    const char *service;
    const char *user;
    const char *rhost;
    int expect;
    /* 1 = only meaningful with no daemon listening (see daemon_is_live). */
    int needs_no_daemon;
};

int main(int argc, char **argv)
{
    if (argc != 3) {
        fprintf(stderr, "usage: %s pam_nirlock.so pam.d/nirlock-lock\n", argv[0]);
        return 2;
    }
    char confdir[] = "/tmp/nirlock-pamtest-XXXXXX";
    if (!mkdtemp(confdir)) {
        perror("mkdtemp");
        return 2;
    }
    char path[512], lane[2048];
    /* DESIGN §4.4 nirlock-lock: the packaged text, module resolved to the built .so. */
    snprintf(path, sizeof path, "%s/nirlock-lock", confdir);
    write_lane_from_packaged(argv[2], path, argv[1]);
    /* The module alone, `required`: pam_authenticate returns its raw code. */
    snprintf(lane, sizeof lane,
             "#%%PAM-1.0\n"
             "auth     required    %s lane=lock timeout=7000 " NO_DAEMON_SOCKET "\n",
             argv[1]);
    snprintf(path, sizeof path, "%s/nirlock-direct", confdir);
    write_file(path, lane);
    snprintf(lane, sizeof lane,
             "#%%PAM-1.0\n"
             "auth     required    %s lane=lock bogus=1 " NO_DAEMON_SOCKET "\n",
             argv[1]);
    snprintf(path, sizeof path, "%s/nirlock-direct-badopt", confdir);
    write_file(path, lane);
    snprintf(lane, sizeof lane,
             "#%%PAM-1.0\n"
             "auth     required    %s lane=sudo " NO_DAEMON_SOCKET "\n",
             argv[1]);
    snprintf(path, sizeof path, "%s/nirlock-direct-badlane", confdir);
    write_file(path, lane);
    /* The packaged `other`, not a copy: a second source of truth here is how
     * the installed one drifted to two types while the repo had four. */
    snprintf(path, sizeof path, "%s/other", confdir);
    {
        char src[512];
        const char *slash = strrchr(argv[2], '/');
        int dirlen = slash ? (int)(slash - argv[2]) : 1;
        snprintf(src, sizeof src, "%.*s/other", dirlen, slash ? argv[2] : ".");
        copy_file(src, path);
    }
    /* Fail-open canaries and option errors. */
    snprintf(lane, sizeof lane,
             "#%%PAM-1.0\n"
             "auth     [success=done maxtries=die default=ignore]  %s lane=lock bogus=1 " NO_DAEMON_SOCKET "\n"
             "auth     required                                    pam_deny.so\n",
             argv[1]);
    snprintf(path, sizeof path, "%s/nirlock-badopt", confdir);
    write_file(path, lane);
    snprintf(lane, sizeof lane,
             "#%%PAM-1.0\n"
             "auth     [success=done maxtries=die default=ignore]  %s lane=lock " NO_DAEMON_SOCKET "\n"
             "auth     optional                                    pam_permit.so\n",
             argv[1]);
    snprintf(path, sizeof path, "%s/canary-permit", confdir);
    write_file(path, lane);
    snprintf(lane, sizeof lane,
             "#%%PAM-1.0\n"
             "auth     sufficient  %s lane=lock " NO_DAEMON_SOCKET "\n"
             "auth     required    pam_deny.so\n",
             argv[1]);
    snprintf(path, sizeof path, "%s/canary-sufficient", confdir);
    write_file(path, lane);

    const struct tcase cases[] = {
        /* Raw codes of §4.3, measured with the module alone as `required`. */
        {"direct: daemon unreachable → AUTHINFO_UNAVAIL (9)", "nirlock-direct", "rodrigo", NULL, PAM_AUTHINFO_UNAVAIL, 0},
        {"direct: invalid PAM_USER → USER_UNKNOWN (10)", "nirlock-direct", "Bad User", NULL, PAM_USER_UNKNOWN, 0},
        {"direct: PAM_USER unset → USER_UNKNOWN (10)", "nirlock-direct", NULL, NULL, PAM_USER_UNKNOWN, 0},
        {"direct: PAM_RHOST set → AUTH_ERR (7)", "nirlock-direct", "rodrigo", "evil.example", PAM_AUTH_ERR, 0},
        {"direct: unknown option → SERVICE_ERR (3)", "nirlock-direct-badopt", "rodrigo", NULL, PAM_SERVICE_ERR, 0},
        {"direct: lane=sudo → SERVICE_ERR (3)", "nirlock-direct-badlane", "rodrigo", NULL, PAM_SERVICE_ERR, 0},
        /* Through the packaged lane: the module returns PAM_AUTHINFO_UNAVAIL
         * (9); default=ignore drops it and pam_deny (required) decides:
         * 7 = PAM_AUTH_ERR. */
        /* Uses the packaged lane verbatim, so it cannot be pointed at a dead
         * socket; skipped when a daemon is listening (see daemon_is_live). */
        {"lane: unavailable → ignore → pam_deny", "nirlock-lock", "rodrigo", NULL, PAM_AUTH_ERR, 1},
        {"lane: invalid user → USER_UNKNOWN → ignore → deny", "nirlock-lock", "Bad User", NULL, PAM_AUTH_ERR, 0},
        {"lane: PAM_RHOST set → AUTH_ERR → ignore → deny", "nirlock-lock", "rodrigo", "evil.example", PAM_AUTH_ERR, 0},
        {"lane: unknown option → SERVICE_ERR → ignore → deny", "nirlock-badopt", "rodrigo", NULL, PAM_AUTH_ERR, 0},
        {"missing service → other → deny", "no-such-service", "rodrigo", NULL, PAM_AUTH_ERR, 0},
        /* Canary: an ignored result followed by `optional pam_permit` is a
         * success (0). This is why the lane must never include system-auth. */
        {"canary: ignore + optional pam_permit fails OPEN", "canary-permit", "rodrigo", NULL, PAM_SUCCESS, 0},
        /* Canary: `sufficient` flattens the module's code to pam_deny's. */
        {"canary: sufficient + pam_deny → 7", "canary-sufficient", "rodrigo", NULL, PAM_AUTH_ERR, 0},
    };
    int bad = 0;
    int live = daemon_is_live();
    for (size_t i = 0; i < sizeof cases / sizeof *cases; i++) {
        if (cases[i].needs_no_daemon && live) {
            printf("skip %-52s    (nirlockd is listening on /run/nirlock/sock)\n", cases[i].name);
            continue;
        }
        int rc = run(confdir, cases[i].service, cases[i].user, cases[i].rhost);
        int ok = rc == cases[i].expect;
        printf("%s %-52s => %2d (%s)%s\n", ok ? "ok  " : "FAIL", cases[i].name, rc,
               rc >= 0 ? pam_strerror(NULL, rc) : "start failed", ok ? "" : " expected different");
        if (!ok)
            bad = 1;
    }
    /* Clean up the confdir. */
    const char *files[] = {"nirlock-lock",  "nirlock-direct", "nirlock-direct-badopt", "nirlock-direct-badlane",
                           "other",         "nirlock-badopt", "canary-permit",         "canary-sufficient"};
    for (size_t i = 0; i < sizeof files / sizeof *files; i++) {
        snprintf(path, sizeof path, "%s/%s", confdir, files[i]);
        unlink(path);
    }
    rmdir(confdir);
    return bad;
}
