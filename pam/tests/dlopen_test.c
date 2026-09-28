/* dlopen()s the built module and checks its exported surface: exactly the
 * two auth symbols, nothing else PAM would call (no account/session/password
 * entry points, §4.1). Usage: dlopen_test <path/to/pam_nirlock.so> */
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>

int main(int argc, char **argv)
{
    if (argc != 2) {
        fprintf(stderr, "usage: %s pam_nirlock.so\n", argv[0]);
        return 2;
    }
    void *h = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
    if (!h) {
        fprintf(stderr, "dlopen: %s\n", dlerror());
        return 1;
    }
    int rc = 0;
    const char *must[] = {"pam_sm_authenticate", "pam_sm_setcred"};
    const char *must_not[] = {"pam_sm_acct_mgmt", "pam_sm_open_session", "pam_sm_close_session", "pam_sm_chauthtok",
                              "parse_opts", "valid_user"};
    for (size_t i = 0; i < sizeof must / sizeof *must; i++) {
        void *s = dlsym(h, must[i]);
        printf("%-22s %s\n", must[i], s ? "present" : "MISSING");
        if (!s)
            rc = 1;
    }
    for (size_t i = 0; i < sizeof must_not / sizeof *must_not; i++) {
        void *s = dlsym(h, must_not[i]);
        printf("%-22s %s\n", must_not[i], s ? "EXPORTED (bad)" : "hidden");
        if (s)
            rc = 1;
    }
    dlclose(h);
    return rc;
}
