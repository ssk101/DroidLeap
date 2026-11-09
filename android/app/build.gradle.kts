plugins {
    id("com.android.application")
}

// Set by CI on release tags; without it the release APK is left unsigned.
val releaseKeystore = providers.environmentVariable("ANDROID_KEYSTORE_FILE").orNull

android {
    namespace = "io.github.ssk101.droidleap"
    compileSdk = 36

    defaultConfig {
        applicationId = "io.github.ssk101.droidleap"
        // Android 11: WindowContext and WindowMetrics, needed for overlays from a service.
        minSdk = 30
        targetSdk = 36
        versionCode = 5
        versionName = "0.5.2"
    }

    signingConfigs {
        if (releaseKeystore != null) {
            create("release") {
                storeFile = file(releaseKeystore)
                storePassword = providers.environmentVariable("ANDROID_KEYSTORE_PASSWORD").get()
                keyAlias = providers.environmentVariable("ANDROID_KEY_ALIAS").get()
                // PKCS12 keystores protect the key with the store password.
                keyPassword = storePassword
            }
        }
    }

    buildTypes {
        release {
            if (releaseKeystore != null) {
                signingConfig = signingConfigs.getByName("release")
            }
            isMinifyEnabled = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"))
        }
    }
}
