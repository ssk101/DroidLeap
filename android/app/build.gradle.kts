plugins {
    id("com.android.application")
}

android {
    namespace = "io.github.ssk101.droidleap"
    compileSdk = 36

    defaultConfig {
        applicationId = "io.github.ssk101.droidleap"
        // Android 11: WindowContext and WindowMetrics, needed for overlays from a service.
        minSdk = 30
        targetSdk = 36
        versionCode = 2
        versionName = "0.2.0"
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"))
        }
    }
}
