use rpt_advanced_standalone::registration;

#[test]
fn registration_worker_reports_pthread_resource_failure() {
    const CHILD: &str = "RPTADV_TEST_REGISTRATION_CHILD";
    const FAULT: &str = "RPTADV_TEST_PTHREAD_FAILURE";
    if std::env::var_os(CHILD).is_some() {
        // SAFETY: the child runs exactly one test on one harness worker; its only
        // new thread request is the synchronous pthread call intercepted below.
        unsafe { std::env::set_var(FAULT, "1") };
        let result = registration::RegistrationWorker::start(Vec::new());
        // SAFETY: the failed request created no thread and the harness is waiting.
        unsafe { std::env::remove_var(FAULT) };
        let error = result.err().expect("pthread EAGAIN must reject the worker");
        assert_eq!(error.raw_os_error(), Some(libc::EAGAIN));
        let mut retry = registration::RegistrationWorker::start(Vec::new())
            .expect("a failed spawn must leave no worker blocking a real retry");
        retry.stop();
        return;
    }
    let directory =
        std::env::temp_dir().join(format!("rptadv-registration-spawn-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("pthread.c");
    let library = directory.join("pthread.so");
    std::fs::write(
        &source,
        r#"
#include <dlfcn.h>
#include <errno.h>
#include <pthread.h>
#include <stdlib.h>
int pthread_create(pthread_t *thread, const pthread_attr_t *attributes,
                   void *(*start)(void *), void *argument) {
    if (getenv("RPTADV_TEST_PTHREAD_FAILURE") != NULL) {
        return EAGAIN;
    }
    int (*canonical)(pthread_t *, const pthread_attr_t *, void *(*)(void *), void *) =
        (int (*)(pthread_t *, const pthread_attr_t *, void *(*)(void *), void *))
        dlsym(RTLD_NEXT, "pthread_create");
    return canonical(thread, attributes, start, argument);
}
"#,
    )
    .unwrap();
    let compiled = std::process::Command::new("cc")
        .args(["-shared", "-fPIC", "-Wall", "-Wextra", "-Werror"])
        .arg(&source)
        .args(["-pthread", "-ldl", "-o"])
        .arg(&library)
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "registration_worker_reports_pthread_resource_failure",
            "--test-threads=1",
            "--nocapture",
        ])
        .env("LD_PRELOAD", library)
        .env(CHILD, "1")
        .output()
        .unwrap();
    assert!(
        child.status.success()
            && String::from_utf8_lossy(&child.stdout).contains("1 passed; 0 failed"),
        "{}{}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
    std::fs::remove_dir_all(directory).unwrap();
}
