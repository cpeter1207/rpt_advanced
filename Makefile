.DEFAULT_GOAL := all
CC ?= cc
AR ?= ar
CPPFLAGS += -Isrc
CFLAGS ?= -O2 -g
WARNINGS := -std=c11 -Wall -Wextra -Wpedantic -Werror
SOURCES := $(wildcard src/*.c)
HEADERS := $(wildcard src/*.h)
TESTS := $(wildcard tests/test_*.c)
OBJECTS := $(patsubst src/%.c,build/%.o,$(SOURCES))
TEST_PROGRAMS := $(patsubst tests/%.c,build/%,$(TESTS))
prefix ?= /usr/local
DESTDIR ?=

.PHONY: all quality lint static-analysis docs check coverage install install-check platform-verify ci clean
all: build/librpt_advanced.a

build:
	mkdir -p $@

build/%.o: src/%.c $(HEADERS) | build
	$(CC) $(CPPFLAGS) $(CFLAGS) $(WARNINGS) -c $< -o $@

build/librpt_advanced.a: $(OBJECTS)
	$(AR) rcs $@ $^

quality: lint static-analysis docs

lint:
	clang-format --dry-run --Werror $(SOURCES) $(HEADERS) $(TESTS)

static-analysis:
	cppcheck --check-level=exhaustive --enable=warning,style,performance,portability --error-exitcode=1 --std=c11 -Isrc $(SOURCES)
	clang-tidy $(SOURCES) --warnings-as-errors='*' -- -Isrc -std=c11

docs: | build
	doxygen Doxyfile

build/test_%: tests/test_%.c $(SOURCES) $(HEADERS) | build
	$(CC) $(CPPFLAGS) $(WARNINGS) -O0 -g --coverage $(SOURCES) $< -o $@

check: $(TEST_PROGRAMS)
	find build -maxdepth 1 -name '*.gcda' -delete
	@set -e; for test in $(TEST_PROGRAMS); do ./$$test; done

coverage: check
	mkdir -p build/coverage
	gcovr --root . --filter 'src/' --fail-under-line 100 --fail-under-branch 100 --xml-pretty -o build/coverage/coverage.xml --print-summary

install: all
	install -d $(DESTDIR)$(prefix)/lib $(DESTDIR)$(prefix)/include/rpt_advanced
	install -m 0644 build/librpt_advanced.a $(DESTDIR)$(prefix)/lib/
	install -m 0644 $(HEADERS) $(DESTDIR)$(prefix)/include/rpt_advanced/

install-check: all
	$(MAKE) DESTDIR=$(CURDIR)/build/stage prefix=/usr install
	cmp build/librpt_advanced.a build/stage/usr/lib/librpt_advanced.a
	cmp src/identifier.h build/stage/usr/include/rpt_advanced/identifier.h
	cmp src/duplex.h build/stage/usr/include/rpt_advanced/duplex.h
	cmp src/config.h build/stage/usr/include/rpt_advanced/config.h
	cmp src/settings.h build/stage/usr/include/rpt_advanced/settings.h
	cmp src/config_reader.h build/stage/usr/include/rpt_advanced/config_reader.h

platform-verify: all coverage install-check

ci: quality platform-verify

clean:
	rm -rf build
