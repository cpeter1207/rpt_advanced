.DEFAULT_GOAL := all
CC ?= cc
AR ?= ar
CPPFLAGS += -Isrc
CFLAGS ?= -O2 -g
WARNINGS := -std=c11 -Wall -Wextra -Wpedantic -Werror
SOURCES := $(wildcard src/*.c)
MODULE_SOURCE := module/app_rpt_advanced.c
MEDIA_SOURCE := module/media.c
SPEECH_SOURCE := module/speech.c
RADIO_SOURCE := module/radio.c
WORKER_SOURCE := module/worker.c
CONNECTION_SOURCE := module/connection.c
RUNTIME_SOURCE := module/runtime.c
MODULE_HELPERS := $(filter-out $(MODULE_SOURCE),$(wildcard module/*.c))
MODULE_OBJECTS := $(patsubst module/%.c,build/module/%.o,$(MODULE_HELPERS))
MODULE_FLAGS := -std=gnu11 -D_GNU_SOURCE -DAST_MODULE_SELF_SYM=ra_module_self -Wall -Wextra -Werror
HEADERS := $(wildcard src/*.h)
TESTS := $(wildcard tests/test_*.c)
OBJECTS := $(patsubst src/%.c,build/%.o,$(SOURCES))
COVERAGE_OBJECTS := $(patsubst src/%.c,build/coverage-objects/%.o,$(SOURCES))
.SECONDARY: $(COVERAGE_OBJECTS)
TEST_PROGRAMS := $(patsubst tests/%.c,build/%,$(TESTS))
prefix ?= /usr/local
multiarch := $(shell $(CC) -print-multiarch)
asteriskmoddir ?= /usr/lib/$(multiarch)/asterisk/modules
DESTDIR ?=

.PHONY: all quality lint static-analysis docs check coverage install install-check integration platform-verify ci clean
all: build/librpt_advanced.a build/app_rpt_advanced.so

build:
	mkdir -p $@

build/%.o: src/%.c $(HEADERS) | build
	$(CC) $(CPPFLAGS) $(CFLAGS) $(WARNINGS) -fPIC -c $< -o $@

build/app_rpt_advanced.o: $(MODULE_SOURCE) $(HEADERS) | build
	$(CC) $(CPPFLAGS) $(CFLAGS) $(MODULE_FLAGS) -fPIC -c $< -o $@

build/module:
	mkdir -p $@

build/module/%.o: module/%.c $(wildcard module/*.h) $(HEADERS) | build/module
	$(CC) $(CPPFLAGS) $(CFLAGS) $(MODULE_FLAGS) -DASTMM_LIBC=ASTMM_IGNORE -fPIC -c $< -o $@

build/app_rpt_advanced.so: build/app_rpt_advanced.o $(OBJECTS) $(MODULE_OBJECTS)
	$(CC) -shared $^ -pthread -lm -o $@

build/librpt_advanced.a: $(OBJECTS)
	$(AR) rcs $@ $^

quality: lint static-analysis docs

lint:
	clang-format --dry-run --Werror $(SOURCES) $(MODULE_SOURCE) $(MODULE_HELPERS) $(wildcard module/*.h) $(HEADERS) $(TESTS) tests/radio_fixture.c
	ruff check tests/*.py
	ruff format --check tests/*.py

static-analysis:
	cppcheck --check-level=exhaustive --enable=warning,style,performance,portability --error-exitcode=1 --std=c11 -Isrc $(SOURCES) $(MODULE_SOURCE) $(MODULE_HELPERS)
	clang-tidy $(SOURCES) --warnings-as-errors='*' -- -Isrc -std=c11
	clang-tidy $(MODULE_SOURCE) $(MODULE_HELPERS) --warnings-as-errors='*' -- -Isrc $(MODULE_FLAGS) -fblocks

docs: | build
	doxygen Doxyfile

build/coverage-objects:
	mkdir -p $@

build/coverage-objects/%.o: src/%.c $(HEADERS) | build/coverage-objects
	$(CC) $(CPPFLAGS) $(WARNINGS) -O0 -g --coverage -fPIC -c $< -o $@

build/module-coverage:
	mkdir -p $@

build/module-coverage/app_rpt_advanced.o: $(MODULE_SOURCE) $(HEADERS) | build/module-coverage
	$(CC) $(CPPFLAGS) $(MODULE_FLAGS) -O0 -g --coverage -fPIC -c $< -o $@

build/module-coverage/app_rpt_advanced.so: build/module-coverage/app_rpt_advanced.o $(COVERAGE_OBJECTS)
	$(CC) --coverage -shared $^ -lm -o $@

build/test_asterisk_module: tests/test_asterisk_module.c build/module-coverage/app_rpt_advanced.so | build
	$(CC) $(MODULE_FLAGS) -Imodule -Isrc -DASTMM_LIBC=ASTMM_IGNORE $< -Wl,--export-dynamic -ldl -o $@

build/module-coverage/runtime.o: $(RUNTIME_SOURCE) $(wildcard module/*.h) $(HEADERS) | build/module-coverage
	$(CC) $(MODULE_FLAGS) -Isrc -DASTMM_LIBC=ASTMM_IGNORE -O0 -g --coverage -fPIC -c $< -o $@

build/test_runtime: tests/test_runtime.c build/module-coverage/runtime.o $(COVERAGE_OBJECTS) | build
	$(CC) $(MODULE_FLAGS) -DASTMM_LIBC=ASTMM_IGNORE -Imodule -Isrc $< build/module-coverage/runtime.o $(COVERAGE_OBJECTS) --coverage -lm -Wl,--wrap=calloc,--wrap=clock_gettime -o $@

build/module-coverage/assets.o: module/assets.c module/assets.h src/speech.h src/settings.h | build/module-coverage
	$(CC) $(MODULE_FLAGS) -Isrc -O0 -g --coverage -fPIC -c $< -o $@

build/test_assets: tests/test_assets.c build/module-coverage/assets.o | build
	$(CC) $(MODULE_FLAGS) -DASTMM_LIBC=ASTMM_IGNORE -Imodule -Isrc $< build/module-coverage/assets.o --coverage \
		-Wl,--wrap=fopen,--wrap=tmpfile,--wrap=mkstemp,--wrap=fseek,--wrap=ftell \
		-Wl,--wrap=fputs,--wrap=fflush,--wrap=fread,--wrap=nanosleep -o $@

build/module-coverage/media.o: $(MEDIA_SOURCE) module/media.h | build/module-coverage
	$(CC) $(MODULE_FLAGS) -O0 -g --coverage -fPIC -c $< -o $@

build/test_asterisk_media: tests/test_asterisk_media.c build/module-coverage/media.o module/media.h | build
	$(CC) $(MODULE_FLAGS) -Imodule $< build/module-coverage/media.o --coverage -o $@

build/module-coverage/radio.o: $(RADIO_SOURCE) module/radio.h | build/module-coverage
	$(CC) $(MODULE_FLAGS) -O0 -g --coverage -fPIC -c $< -o $@

build/test_radio: tests/test_radio.c build/module-coverage/radio.o module/radio.h | build
	$(CC) $(MODULE_FLAGS) -Imodule $< build/module-coverage/radio.o --coverage -o $@

build/module-coverage/connection.o: $(CONNECTION_SOURCE) module/connection.h module/media.h module/radio.h | build/module-coverage
	$(CC) $(MODULE_FLAGS) -O0 -g --coverage -fPIC -c $< -o $@

build/test_connection: tests/test_connection.c build/module-coverage/connection.o module/connection.h | build
	$(CC) $(MODULE_FLAGS) -Imodule $< build/module-coverage/connection.o --coverage -Wl,--wrap=ra_media_select -o $@

build/module-coverage/worker.o: $(WORKER_SOURCE) module/worker.h $(HEADERS) | build/module-coverage
	$(CC) $(MODULE_FLAGS) -Isrc -O0 -g --coverage -fPIC -c $< -o $@

build/test_worker: tests/test_worker.c build/module-coverage/worker.o $(COVERAGE_OBJECTS) module/worker.h | build
	$(CC) $(MODULE_FLAGS) -Imodule -Isrc $< build/module-coverage/worker.o $(COVERAGE_OBJECTS) --coverage -pthread -lm \
		-Wl,--wrap=pthread_create,--wrap=pthread_join,--wrap=clock_gettime,--wrap=ra_radio_exchange -o $@

build/test_worker_thread: tests/test_worker_thread.c build/module-coverage/worker.o $(COVERAGE_OBJECTS) module/worker.h | build
	$(CC) $(MODULE_FLAGS) -Imodule -Isrc $< build/module-coverage/worker.o $(COVERAGE_OBJECTS) --coverage -pthread -lm -Wl,--wrap=ra_radio_exchange -o $@

build/module-coverage/speech.o: $(SPEECH_SOURCE) src/speech.h | build/module-coverage
	$(CC) $(MODULE_FLAGS) -Isrc -O0 -g --coverage -fPIC -c $< -o $@

build/test_speech: tests/test_speech.c build/module-coverage/speech.o src/speech.h | build
	$(CC) $(MODULE_FLAGS) -Isrc $< build/module-coverage/speech.o --coverage \
		-Wl,--wrap=posix_spawn_file_actions_init,--wrap=posix_spawn_file_actions_adddup2 \
		-Wl,--wrap=posix_spawn_file_actions_addclose,--wrap=posix_spawn_file_actions_destroy \
		-Wl,--wrap=posix_spawnp,--wrap=waitpid,--wrap=kill -o $@

build/test_speech_process: tests/test_speech_process.c build/module-coverage/speech.o build/module-coverage/assets.o src/speech.h | build
	$(CC) $(WARNINGS) -Isrc -Imodule $< build/module-coverage/speech.o build/module-coverage/assets.o --coverage -o $@

build/test_%: tests/test_%.c $(COVERAGE_OBJECTS) $(HEADERS) | build
	$(CC) $(CPPFLAGS) $(WARNINGS) -O0 -g --coverage $< $(COVERAGE_OBJECTS) -lm -o $@

build/test_document: tests/test_document.c $(COVERAGE_OBJECTS) $(HEADERS) | build
	$(CC) $(CPPFLAGS) $(WARNINGS) -O0 -g --coverage $< $(COVERAGE_OBJECTS) -Wl,--wrap=strdup,--wrap=reallocarray -lm -o $@

check: $(TEST_PROGRAMS)
	find build -name '*.gcda' -delete
	@set -e; for test in $(TEST_PROGRAMS); do ./$$test; done

coverage: check
	mkdir -p build/coverage
	gcovr --root . --filter 'src/|module/' --fail-under-line 100 --fail-under-branch 100 --xml-pretty -o build/coverage/coverage.xml --print-summary

install: all
	install -d $(DESTDIR)$(asteriskmoddir)
	install -m 0755 build/app_rpt_advanced.so $(DESTDIR)$(asteriskmoddir)/
	install -d $(DESTDIR)$(prefix)/lib $(DESTDIR)$(prefix)/include/rpt_advanced
	install -d $(DESTDIR)$(prefix)/share/doc/rpt_advanced
	install -m 0644 COPYING $(DESTDIR)$(prefix)/share/doc/rpt_advanced/copyright
	install -d $(DESTDIR)$(prefix)/share/doc/rpt_advanced/examples
	install -m 0644 examples/rpt_advanced.conf $(DESTDIR)$(prefix)/share/doc/rpt_advanced/examples/
	install -m 0644 build/librpt_advanced.a $(DESTDIR)$(prefix)/lib/
	install -m 0644 $(HEADERS) $(DESTDIR)$(prefix)/include/rpt_advanced/

install-check: all
	$(MAKE) DESTDIR=$(CURDIR)/build/stage prefix=/usr install
	cmp build/librpt_advanced.a build/stage/usr/lib/librpt_advanced.a
	cmp src/identifier.h build/stage/usr/include/rpt_advanced/identifier.h
	cmp src/duplex.h build/stage/usr/include/rpt_advanced/duplex.h
	cmp src/controller.h build/stage/usr/include/rpt_advanced/controller.h
	cmp src/morse.h build/stage/usr/include/rpt_advanced/morse.h
	cmp src/playback.h build/stage/usr/include/rpt_advanced/playback.h
	cmp src/config.h build/stage/usr/include/rpt_advanced/config.h
	cmp src/settings.h build/stage/usr/include/rpt_advanced/settings.h
	cmp src/config_reader.h build/stage/usr/include/rpt_advanced/config_reader.h
	cmp src/document.h build/stage/usr/include/rpt_advanced/document.h
	cmp src/schema.h build/stage/usr/include/rpt_advanced/schema.h
	cmp src/speech.h build/stage/usr/include/rpt_advanced/speech.h
	cmp COPYING build/stage/usr/share/doc/rpt_advanced/copyright
	cmp examples/rpt_advanced.conf build/stage/usr/share/doc/rpt_advanced/examples/rpt_advanced.conf
	cmp build/app_rpt_advanced.so build/stage$(asteriskmoddir)/app_rpt_advanced.so

build/chan_rpt_fixture.so: tests/radio_fixture.c | build
	$(CC) $(MODULE_FLAGS) -O2 -g -fPIC -shared $< -pthread -o $@

integration: install-check build/chan_rpt_fixture.so
	RPT_TEST_MODULE_DIR="$(CURDIR)/build/stage$(asteriskmoddir)" python3 tests/test_asterisk_integration.py

platform-verify: all coverage install-check integration

ci: quality platform-verify

clean:
	rm -rf build
