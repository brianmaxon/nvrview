import NVRViewApp

// The shipped app: the app code lives in NVRViewApp, and this build links no
// self-test (the self-test build is NVRVIEW_SELFTEST=1; see Package.swift).
#if NVRVIEW_SELFTEST
import NVRViewSelfTest
#endif

runUpdateHelperIfAsked()
#if NVRVIEW_SELFTEST
MainActor.assumeIsolated { runApp(selfTest: selfTestHook()) }
#else
MainActor.assumeIsolated { runApp() }
#endif
