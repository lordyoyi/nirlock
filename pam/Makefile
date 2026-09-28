# pam_nirlock.so — build and tests (DESIGN §4.5, §10).
#
#   make            builds build/pam_nirlock.so
#   make test       dlopen symbol check + lane harness through the real libpam
#   make install    DESTDIR/usr/lib/security/pam_nirlock.so

CC      ?= cc
BUILD   ?= build
DESTDIR ?=
LIBDIR  ?= /usr/lib

CFLAGS  ?= -O2
CFLAGS  += -DNIRLOCK_VERSION='"$(VERSION)"' -std=c11 -fPIC -fvisibility=hidden -D_FORTIFY_SOURCE=3 -fstack-protector-strong \
           -Wall -Wextra -Werror -Wformat=2 -Wconversion -Wno-sign-conversion
LDFLAGS += -shared -Wl,-z,relro,-z,now,-z,noexecstack -Wl,--no-undefined
LDLIBS  += -lpam

MODULE  := $(BUILD)/pam_nirlock.so
SRC     := pam_nirlock.c nirlock_wire.c
VERSION ?= 0.1.0

all: $(MODULE)

$(BUILD):
	mkdir -p $(BUILD)

$(MODULE): $(SRC) nirlock_wire.h | $(BUILD)
	$(CC) $(CFLAGS) $(LDFLAGS) -o $@ $(SRC) $(LDLIBS)

$(BUILD)/dlopen_test: tests/dlopen_test.c | $(BUILD)
	$(CC) -O2 -Wall -Wextra -Werror -o $@ $< -ldl

$(BUILD)/lane_test: tests/lane_test.c | $(BUILD)
	$(CC) -O2 -Wall -Wextra -Werror -o $@ $< -lpam

# DESIGN §10: `PAM_IGNORE` and `pam_get_user` may appear in pam/*.c only on
# the two guard lines, exactly `#undef PAM_IGNORE` and
# `#pragma GCC poison PAM_IGNORE pam_get_user` (comments included in the ban;
# the compile-time poison is the real guard, this keeps the grep honest).
check-source:
	@! grep -n 'PAM_IGNORE\|pam_get_user' pam_nirlock.c \
	    | grep -v '^[0-9]*:#undef PAM_IGNORE$$' \
	    | grep -v '^[0-9]*:#pragma GCC poison PAM_IGNORE pam_get_user$$' \
	    | grep . || (echo "forbidden identifier in $(SRC)"; exit 1)
	@test "$$(grep -c PAM_IGNORE pam_nirlock.c)" = 2 || (echo "expected exactly the two guard lines"; exit 1)
	@echo "check-source: ok"

# The lane harness reads the packaged lane text (pam.d/nirlock-lock) and
# substitutes only the module token, so drift between the shipped file and
# the test is impossible.
test: $(MODULE) $(BUILD)/dlopen_test $(BUILD)/lane_test check-source
	./$(BUILD)/dlopen_test $(abspath $(MODULE))
	./$(BUILD)/lane_test $(abspath $(MODULE)) $(abspath ../pam.d/nirlock-lock)

install: $(MODULE)
	install -Dm755 $(MODULE) $(DESTDIR)$(LIBDIR)/security/pam_nirlock.so

clean:
	rm -rf $(BUILD)

.PHONY: all test check-source install clean
