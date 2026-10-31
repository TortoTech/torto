import groovy.json.JsonOutput
plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
}
val repositoryRoot = rootDir.resolve("../..").canonicalFile
val appVersion = Regex("""(?m)^version = ["]([0-9]+\.[0-9]+\.[0-9]+)["]""").find(repositoryRoot.resolve("Cargo.toml").readText())!!.groupValues[1]
val verifierVersion=Regex("""name = "rustls-platform-verifier-android"\s+version = ["]([^"]+)["]""").find(repositoryRoot.resolve("Cargo.lock").readText())!!.groupValues[1]
val versionParts = appVersion.split(".").map(String::toInt)
require(versionParts[0] <= 2000 && versionParts[1] < 1000 && versionParts[2] < 1000)
val appVersionCode = versionParts[0] * 1_000_000 + versionParts[1] * 1_000 + versionParts[2]
val rustAbis = providers.gradleProperty("rustAbis").orElse("arm64-v8a").get().split(",")
require(rustAbis.isNotEmpty() && rustAbis.distinct().size == rustAbis.size && rustAbis.all { it in setOf("arm64-v8a", "x86_64") }) { "Unsupported or duplicate Android ABI" }
// Exercise the production Rust library in the isolated, debug-signed preview.
val releaseNativePreview = providers.gradleProperty("releaseNativePreview").orElse("false").get().toBooleanStrict()
// Optional direct-download artifact; default mmap loading avoids a second copy
// of native libraries on the device. Compressed APKs extract them at install.
val compressNativeLibraries = providers.gradleProperty("compressNativeLibraries").orElse("false").get().toBooleanStrict()
fun nativeVariant(variant: String) = if (variant == "debug" && releaseNativePreview) "release" else variant
val rustTasks = listOf("debug", "release").associateWith { variant ->
    tasks.register<Exec>("buildRust" + variant.replaceFirstChar(Char::uppercase)) {
        workingDir(repositoryRoot)
        commandLine(listOf("python3", "scripts/build-android.py", "--abis", rustAbis.joinToString(",")) + if (variant == "release") listOf("--release") else emptyList())
        inputs.files(fileTree(repositoryRoot.resolve("crates")) { include("**/*.rs", "**/*.toml") })
        inputs.files(repositoryRoot.resolve("Cargo.toml"), repositoryRoot.resolve("Cargo.lock"), repositoryRoot.resolve("rust-toolchain.toml"))
        inputs.dir(repositoryRoot.resolve("third_party"))
        inputs.dir(repositoryRoot.resolve("assets"))
        inputs.file(repositoryRoot.resolve("scripts/build-android.py"))
        inputs.property("abis", rustAbis.joinToString(","))
        inputs.property("profile", variant)
        outputs.dir(layout.buildDirectory.dir("generated/rust/$variant"))
    }
}
val brandResources = tasks.register<Copy>("prepareBrandResources") {
    from(repositoryRoot.resolve("assets/macos/torto-128.png"))
    into(layout.buildDirectory.dir("generated/brand/drawable"))
    rename { "ic_torto.png" }
}
tasks.named("preBuild").configure { dependsOn(brandResources) }
val notices = tasks.register<Exec>("generateOpenSourceNotices") {
    val metadataFile=layout.buildDirectory.file("generated/notices/artifacts.json")
    val outputFile=layout.buildDirectory.file("generated/notices/assets/open-source.json")
    inputs.file(repositoryRoot.resolve("scripts/android-notices.py"))
    inputs.files(repositoryRoot.resolve("Cargo.lock"),repositoryRoot.resolve("Cargo.toml"),repositoryRoot.resolve("LICENSE"),file("build.gradle.kts"))
    outputs.file(outputFile)
    workingDir(repositoryRoot)
    doFirst {
        val artifacts=configurations.getByName("releaseRuntimeClasspath").resolvedConfiguration.resolvedArtifacts.map {artifact->mapOf("group" to artifact.moduleVersion.id.group,"name" to artifact.moduleVersion.id.name,"version" to artifact.moduleVersion.id.version,"file" to artifact.file.absolutePath)}
        metadataFile.get().asFile.apply {parentFile.mkdirs();writeText(JsonOutput.toJson(artifacts))}
        commandLine("python3","scripts/android-notices.py","--artifacts",metadataFile.get().asFile.absolutePath,"--gradle-home",gradle.gradleUserHomeDir.absolutePath,"--output",outputFile.get().asFile.absolutePath)
    }
}
tasks.named("preBuild").configure {dependsOn(notices)}
val signingFile = providers.environmentVariable("TORTO_ANDROID_KEYSTORE").orNull
android {
    namespace = "org.tortotech.torto"
    compileSdk = 36
    ndkVersion = "28.2.13676358"
    defaultConfig {
        applicationId = "org.tortotech.torto"
        resValue("string", "app_name", "Torto")
        minSdk = 26
        targetSdk = 36
        versionCode = appVersionCode
        versionName = appVersion
        if (rustAbis.size == 1) ndk { abiFilters += rustAbis }
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }
    if (signingFile != null) signingConfigs.create("distribution") {
        storeFile = file(signingFile)
        storePassword = providers.environmentVariable("TORTO_ANDROID_STORE_PASSWORD").get()
        keyAlias = providers.environmentVariable("TORTO_ANDROID_KEY_ALIAS").get()
        keyPassword = providers.environmentVariable("TORTO_ANDROID_KEY_PASSWORD").get()
    }
    buildTypes {
        debug { applicationIdSuffix = ".debug"; versionNameSuffix = "-dev"; resValue("string", "app_name", "Torto Preview") }
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            ndk.debugSymbolLevel = "FULL"
            if (signingFile != null) signingConfig = signingConfigs.getByName("distribution")
        }
    }
    compileOptions { sourceCompatibility = JavaVersion.VERSION_17; targetCompatibility = JavaVersion.VERSION_17 }
    splits {
        abi {
            isEnable = rustAbis.size > 1
            reset()
            include(*rustAbis.toTypedArray())
            isUniversalApk = false
        }
    }

    buildFeatures { compose = true; resValues = true }
    androidResources { localeFilters += setOf("zh", "en") }
    sourceSets["main"].assets.srcDir(layout.buildDirectory.dir("generated/notices/assets"))
    sourceSets["main"].res.srcDir(layout.buildDirectory.dir("generated/brand"))
    listOf("debug", "release").forEach { variant ->
        val native = nativeVariant(variant)
        sourceSets[variant].java.srcDir(layout.buildDirectory.dir("generated/rust/$native/kotlin"))
        sourceSets[variant].jniLibs.srcDir(layout.buildDirectory.dir("generated/rust/$native/jniLibs"))
    }
    packaging { jniLibs { useLegacyPackaging = compressNativeLibraries } }
}
listOf("debug", "release").forEach { variant ->
    tasks.matching { it.name == "pre" + variant.replaceFirstChar(Char::uppercase) + "Build" }.configureEach { dependsOn(rustTasks.getValue(nativeVariant(variant))) }
}
dependencies {
    implementation(platform("androidx.compose:compose-bom:2025.12.01"))
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.foundation:foundation")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.activity:activity-compose:1.12.4")
    implementation("androidx.lifecycle:lifecycle-viewmodel-compose:2.10.0")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.10.0")
    implementation("androidx.customview:customview:1.2.0")
    implementation("androidx.work:work-runtime-ktx:2.12.0")
    implementation("net.java.dev.jna:jna:5.19.1@aar")
    implementation("org.rustls:rustls-platform-verifier:$verifierVersion")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.10.2")
    testImplementation("junit:junit:4.13.2")
    androidTestImplementation("androidx.test:runner:1.7.0")
    androidTestImplementation("androidx.test.ext:junit:1.3.0")
}

kotlin { compilerOptions { jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17) } }
