import java.util.Properties

plugins {
    id("com.android.application")
    // The Flutter Gradle Plugin must be applied after the Android and Kotlin Gradle plugins.
    id("dev.flutter.flutter-gradle-plugin")
}

val releaseProperties = providers.environmentVariable("NEXDESK_ANDROID_SIGNING_PROPERTIES")
    .orNull?.let { file(it).canonicalFile }

android {
    namespace = "dev.nexdesk.nexdesk_desktop"
    compileSdk = flutter.compileSdkVersion

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    defaultConfig {
        applicationId = "dev.nexdesk.nexdesk_desktop"
        minSdk = flutter.minSdkVersion
        targetSdk = flutter.targetSdkVersion
        // Uses the version code from pubspec.yaml. When using split APKs, 1000 * ABI_VERSION
        // is added automatically by Flutter. (https://developer.android.com/studio/build/configure-apk-splits#configure-APK-versions)
        // You can force using the value of versionCode by specifying the `-P force-version-code-ignoring-abi=true`
        // flag during build.
        versionCode = flutter.versionCode
        versionName = flutter.versionName
    }

    signingConfigs {
        releaseProperties?.let { propertiesFile ->
            val values = Properties().apply { propertiesFile.inputStream().use { load(it) } }
            fun value(name: String): String =
                requireNotNull(values.getProperty(name)?.takeIf(String::isNotBlank)) {
                    "Missing Android signing property: $name"
                }
            create("nexdeskRelease") {
                storeFile = propertiesFile.parentFile.resolve(value("storeFile"))
                storePassword = value("storePassword")
                keyAlias = value("keyAlias")
                keyPassword = value("keyPassword")
            }
        }
    }

    buildTypes {
        release {
            signingConfig = signingConfigs.findByName("nexdeskRelease")
        }
    }
}

kotlin {
    compilerOptions {
        jvmTarget = org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17
    }
}

flutter {
    source = "../.."
}
