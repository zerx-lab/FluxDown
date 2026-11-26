import java.io.ByteArrayOutputStream
import java.io.StringReader
import java.util.Properties
import javax.inject.Inject

plugins {
    alias(libs.plugins.android.library)
}

/**
 * :bridge —— UniFFI `native/mobile`（crate `fluxdown_mobile`）的 Android 侧：
 *  1. `cargoNdk<Variant>`：cargo-ndk 把 Rust 编成各 ABI 的 `libfluxdown_mobile.so`，作为该变体的 jniLibs 生成源；
 *  2. `uniffiBindgen<Variant>`：从 `cargoNdkBindingsLib`（debug、首个 ABI、未 strip，所有变体共用）的 .so 里的
 *     proc-macro 元数据生成 Kotlin 绑定，作为该变体的 Kotlin 生成源（release 的 .so 被 `strip = true` 抹掉了元数据）；
 *  3. `RustHostSession` / `FluxBridge`（src/main）：把生成的绑定适配成 `:core` 的 `HostSession` 端口。
 *
 * 构建参数（均为可选）：
 *  - Gradle 属性 `fluxdown.abis`：逗号分隔的 ABI 列表，默认 `arm64-v8a,x86_64`；
 *  - `local.properties` 的 `ndk.dir`，其次环境变量 `ANDROID_NDK_HOME`，最后 `<sdk>/ndk/` 下版本号最高者；
 *  - Gradle 属性 `fluxdown.cargoBin`：cargo 可执行文件所在目录（IDE 启动的 Gradle 不一定带 ~/.cargo/bin）。
 * 前置：`rustup target add aarch64-linux-android x86_64-linux-android`、`cargo install cargo-ndk`。
 */

/** 构建期共用常量与环境解析（task 执行期调用；脚本顶层成员对嵌套 task 类不可见，故放在 object 里）。 */
object Rust {
    const val PACKAGE = "fluxdown_mobile"
    const val LIB = "lib$PACKAGE.so"

    fun ndkHome(localProperties: String?, sdkDir: File): File {
        val fromLocal = localProperties
            ?.let { text -> Properties().apply { load(StringReader(text)) }.getProperty("ndk.dir") }
            ?.takeIf { it.isNotBlank() }
        val fromEnv = System.getenv("ANDROID_NDK_HOME")?.takeIf { it.isNotBlank() }
        val explicit = fromLocal ?: fromEnv
        if (explicit != null) return File(explicit)
        fun versionKey(name: String): List<Int> = name.split('.').map { it.toIntOrNull() ?: 0 }
        val latest = File(sdkDir, "ndk").listFiles { f -> File(f, "source.properties").isFile }
            ?.maxWithOrNull { a, b ->
                val ka = versionKey(a.name)
                val kb = versionKey(b.name)
                (0 until maxOf(ka.size, kb.size))
                    .map { ka.getOrElse(it) { 0 }.compareTo(kb.getOrElse(it) { 0 }) }
                    .firstOrNull { it != 0 } ?: 0
            }
        return latest ?: throw GradleException(
            "未找到 Android NDK：请在 local.properties 设置 ndk.dir，或安装 NDK 到 ${File(sdkDir, "ndk")}（或设置 ANDROID_NDK_HOME）",
        )
    }

    /** cargo / cargo-ndk 所在目录（按优先级）：显式目录 → $CARGO_HOME/bin → ~/.cargo/bin。 */
    private fun cargoBins(explicitBin: String?): List<String> = listOfNotNull(
        explicitBin?.takeIf { it.isNotBlank() },
        System.getenv("CARGO_HOME")?.takeIf { it.isNotBlank() }?.let { "$it/bin" },
        "${System.getProperty("user.home")}/.cargo/bin",
    ).filter { File(it).isDirectory }

    /** 供 cargo-ndk 再拉起 `cargo-ndk` / rustc 的 PATH。 */
    fun cargoPath(explicitBin: String?): String =
        (cargoBins(explicitBin) + listOfNotNull(System.getenv("PATH"))).joinToString(File.pathSeparator)

    /** Gradle 守护进程的 PATH 常不含 ~/.cargo/bin（IDE 启动时尤甚），可执行文件必须用绝对路径。 */
    fun cargo(explicitBin: String?): String =
        cargoBins(explicitBin).map { File(it, "cargo") }.firstOrNull { it.isFile }?.absolutePath ?: "cargo"
}

abstract class CargoNdkBuild @Inject constructor(private val exec: ExecOperations) : DefaultTask() {
    /** 仓库根（Cargo workspace 根，cargo 的工作目录）。 */
    @get:Internal
    abstract val workspaceRoot: DirectoryProperty

    /** Rust sources and local Cargo patches used to invalidate the native library. */
    @get:InputFiles
    @get:PathSensitive(PathSensitivity.RELATIVE)
    abstract val rustSources: ConfigurableFileCollection

    @get:Input
    abstract val abis: Property<String>

    @get:Input
    abstract val release: Property<Boolean>

    @get:Input
    abstract val platform: Property<Int>

    @get:Input
    @get:Optional
    abstract val localProperties: Property<String>

    @get:Input
    abstract val sdkDir: Property<String>

    @get:Input
    @get:Optional
    abstract val cargoBin: Property<String>

    /**
     * 核心经 `option_env!` 读取的编译期环境变量（名 → 值，空串 = 未设置）：作为输入，值变化时任务重跑，
     * 否则 Gradle 判定 up-to-date，沿用旧值编出的 .so。
     */
    @get:Input
    abstract val compileEnv: MapProperty<String, String>

    @get:OutputDirectory
    abstract val outputDir: DirectoryProperty

    @TaskAction
    fun build() {
        val out = outputDir.get().asFile
        out.deleteRecursively()
        out.mkdirs()
        val ndk = Rust.ndkHome(localProperties.orNull, File(sdkDir.get()))
        val abiList = abis.get().split(',').map { it.trim() }.filter { it.isNotEmpty() }
        require(abiList.isNotEmpty()) { "fluxdown.abis 为空" }
        val args = buildList {
            add(Rust.cargo(cargoBin.orNull))
            add("ndk")
            abiList.forEach { add("-t"); add(it) }
            add("--platform"); add(platform.get().toString())
            add("-o"); add(out.absolutePath)
            add("build")
            add("-p"); add(Rust.PACKAGE)
            if (release.get()) add("--release")
        }
        exec.exec {
            workingDir = workspaceRoot.get().asFile
            environment("ANDROID_NDK_HOME", ndk.absolutePath)
            environment("PATH", Rust.cargoPath(cargoBin.orNull))
            // 空串必须移除：option_env! 会把空串当成「已设置」写进二进制。
            compileEnv.get().forEach { (name, value) ->
                if (value.isEmpty()) environment.remove(name) else environment(name, value)
            }
            commandLine(args)
        }
        abiList.forEach { abi ->
            val dir = File(out, abi)
            check(File(dir, Rust.LIB).isFile) { "cargo-ndk 未产出 $abi/${Rust.LIB}" }
            // cargo-ndk 会连带复制依赖 crate 的 cdylib 产物（如 libts2mp4-*.so）：只打包桥本身。
            dir.listFiles { f -> f.extension == "so" && f.name != Rust.LIB }?.forEach { it.delete() }
        }
    }
}

abstract class UniffiBindgen @Inject constructor(private val exec: ExecOperations) : DefaultTask() {
    @get:Internal
    abstract val workspaceRoot: DirectoryProperty

    /** cargo-ndk 产出（.so 内嵌 proc-macro 元数据）。 */
    @get:InputDirectory
    @get:PathSensitive(PathSensitivity.RELATIVE)
    abstract val jniLibs: DirectoryProperty

    /** `native/mobile/uniffi.toml`：包名 / cdylib 名配置。 */
    @get:InputFiles
    @get:PathSensitive(PathSensitivity.RELATIVE)
    abstract val bindgenConfig: ConfigurableFileCollection

    @get:Input
    abstract val abis: Property<String>

    @get:Input
    @get:Optional
    abstract val cargoBin: Property<String>

    @get:OutputDirectory
    abstract val outputDir: DirectoryProperty

    @TaskAction
    fun generate() {
        val out = outputDir.get().asFile
        out.deleteRecursively()
        out.mkdirs()
        val abi = abis.get().split(',').map { it.trim() }.first { it.isNotEmpty() }
        val library = File(jniLibs.get().asFile, "$abi/${Rust.LIB}")
        check(library.isFile) { "缺少 $library" }
        exec.exec {
            workingDir = workspaceRoot.get().asFile
            environment("PATH", Rust.cargoPath(cargoBin.orNull))
            commandLine(
                Rust.cargo(cargoBin.orNull), "run", "-p", Rust.PACKAGE, "--features", "bindgen", "--bin", "uniffi-bindgen", "--",
                "generate", "--library", library.absolutePath,
                "--language", "kotlin", "--no-format", "--out-dir", out.absolutePath,
            )
        }
    }
}

/**
 * 宿主机（macOS dylib / Linux .so / Windows dll）的 `libfluxdown_mobile`：给 JVM 单元测试经 JNA 加载，
 * 与 `cargoNdk*` 同源（同一份 `native/` 源码 → UniFFI 元数据与校验和一致，生成的 Kotlin 绑定可直接对上）。
 * 构建失败即任务失败：冒烟测试不会在“没有 Rust 库”时静默通过。
 */
abstract class CargoHostBuild @Inject constructor(private val exec: ExecOperations) : DefaultTask() {
    @get:Internal
    abstract val workspaceRoot: DirectoryProperty

    @get:InputFiles
    @get:PathSensitive(PathSensitivity.RELATIVE)
    abstract val rustSources: ConfigurableFileCollection

    @get:Input
    @get:Optional
    abstract val cargoBin: Property<String>

    @get:OutputDirectory
    abstract val outputDir: DirectoryProperty

    @TaskAction
    fun build() {
        val out = outputDir.get().asFile
        out.deleteRecursively()
        out.mkdirs()
        val cargo = Rust.cargo(cargoBin.orNull)
        val path = Rust.cargoPath(cargoBin.orNull)
        exec.exec {
            workingDir = workspaceRoot.get().asFile
            environment("PATH", path)
            commandLine(cargo, "build", "-p", Rust.PACKAGE)
        }
        // 目标目录可被 CARGO_TARGET_DIR / .cargo/config.toml 改写：以 cargo 自己报告的为准。
        val metadata = ByteArrayOutputStream()
        exec.exec {
            workingDir = workspaceRoot.get().asFile
            environment("PATH", path)
            commandLine(cargo, "metadata", "--format-version", "1", "--no-deps")
            standardOutput = metadata
        }
        val targetDir = Regex("\"target_directory\":\"((?:[^\"\\\\]|\\\\.)*)\"")
            .find(metadata.toString(Charsets.UTF_8))
            ?.groupValues?.get(1)
            ?.replace("\\\\", "\\")
            ?.replace("\\/", "/")
            ?: throw GradleException("cargo metadata 未报告 target_directory")
        val osName = System.getProperty("os.name").lowercase()
        val fileName = when {
            "mac" in osName -> "lib${Rust.PACKAGE}.dylib"
            "win" in osName -> "${Rust.PACKAGE}.dll"
            else -> "lib${Rust.PACKAGE}.so"
        }
        val library = File(targetDir, "debug/$fileName")
        check(library.isFile) { "cargo build 未产出宿主库 $library" }
        library.copyTo(File(out, fileName), overwrite = true)
    }
}

android {
    namespace = "com.fluxdown.bridge"
    compileSdk {
        version = release(37)
    }

    defaultConfig {
        minSdk = 26
        consumerProguardFiles("consumer-rules.pro")
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

private val repoRoot = rootProject.layout.projectDirectory.dir("../..")
private val abiProperty = providers.gradleProperty("fluxdown.abis").orElse("arm64-v8a,x86_64")
private val cargoBinProperty = providers.gradleProperty("fluxdown.cargoBin")

fun CargoNdkBuild.configureCommon() {
    workspaceRoot.set(repoRoot)
    rustSources.from(
        fileTree(repoRoot) {
            include("Cargo.toml", "Cargo.lock", ".cargo/config.toml", "native/**", "third_party/**")
            exclude("**/target/**")
        },
    )
    // 引擎库按 minSdk 链接 NDK 桩：高于 minSdk 会在旧系统上加载时报符号缺失
    platform.set(android.defaultConfig.minSdk ?: error("minSdk 未设置"))
    localProperties.set(
        providers.fileContents(rootProject.layout.projectDirectory.file("local.properties")).asText,
    )
    sdkDir.set(androidComponents.sdkComponents.sdkDirectory.map { it.asFile.absolutePath })
    cargoBin.set(cargoBinProperty)
    // FLUXCLOUD_BASE_URL：agent 嵌入模式的云端地址；FLUXDOWN_APP_VERSION：引擎 UA 等编译期版本（native/engine/build.rs）
    listOf("FLUXCLOUD_BASE_URL", "FLUXDOWN_APP_VERSION").forEach { name ->
        compileEnv.put(name, providers.environmentVariable(name).orElse(""))
    }
}

// release 配置 `strip = true` 会抹掉 UniFFI proc-macro 元数据符号，绑定只能从未 strip 的库生成；
// 绑定与优化级别无关，所有变体共用这一个 debug、单 ABI 的元数据库（cargo 增量，几乎零成本）。
val cargoNdkBindingsLib = tasks.register<CargoNdkBuild>("cargoNdkBindingsLib") {
    description = "cargo-ndk：编译 ${Rust.PACKAGE}（debug，首个 ABI）供 uniffi-bindgen 读取元数据"
    configureCommon()
    abis.set(abiProperty.map { it.substringBefore(',').trim() })
    release.set(false)
    outputDir.set(layout.buildDirectory.dir("intermediates/uniffi/bindingsLib"))
}

// JVM 单元测试经 JNA 加载的宿主机库（见 CargoHostBuild）；测试任务把它的目录作为 jna.library.path。
val hostLibDir = layout.buildDirectory.dir("intermediates/uniffi/hostLib")
val cargoHostLib = tasks.register<CargoHostBuild>("cargoHostLib") {
    description = "cargo build：编译 ${Rust.PACKAGE} 的宿主机库供 JVM 单元测试（JNA）加载"
    workspaceRoot.set(repoRoot)
    rustSources.from(
        fileTree(repoRoot) {
            include("Cargo.toml", "Cargo.lock", ".cargo/config.toml", "native/**", "third_party/**")
            exclude("**/target/**")
        },
    )
    cargoBin.set(cargoBinProperty)
    outputDir.set(hostLibDir)
}

tasks.withType<Test>().configureEach {
    dependsOn(cargoHostLib)
    inputs.dir(hostLibDir).withPropertyName("hostLib")
    systemProperty("jna.library.path", hostLibDir.get().asFile.absolutePath)
    jvmArgs("--enable-native-access=ALL-UNNAMED")
    // 冒烟测试跑真实引擎：要看到每个用例的结果与耗时，而不只是汇总。
    testLogging {
        events("passed", "failed", "skipped")
        showExceptions = true
        showStandardStreams = true
    }
}

androidComponents {
    onVariants { variant ->
        val cap = variant.name.replaceFirstChar { it.uppercase() }

        val cargoNdk = tasks.register<CargoNdkBuild>("cargoNdk$cap") {
            description = "cargo-ndk：编译 ${Rust.PACKAGE}（${variant.name}）为 jniLibs"
            configureCommon()
            abis.set(abiProperty)
            release.set(variant.buildType == "release")
        }
        val bindgen = tasks.register<UniffiBindgen>("uniffiBindgen$cap") {
            description = "uniffi-bindgen：生成 ${Rust.PACKAGE} 的 Kotlin 绑定（${variant.name}）"
            workspaceRoot.set(repoRoot)
            jniLibs.set(cargoNdkBindingsLib.flatMap { it.outputDir })
            bindgenConfig.from(repoRoot.file("native/mobile/uniffi.toml"))
            abis.set(abiProperty.map { it.substringBefore(',').trim() })
            cargoBin.set(cargoBinProperty)
        }
        variant.sources.jniLibs?.addGeneratedSourceDirectory(cargoNdk, CargoNdkBuild::outputDir)
        variant.sources.kotlin?.addGeneratedSourceDirectory(bindgen, UniffiBindgen::outputDir)
    }
}

dependencies {
    api(project(":core"))
    // UniFFI Kotlin 绑定经 JNA 调用 libfluxdown_mobile.so；Android 必须用 aar（内含各 ABI 的 libjnidispatch.so）。
    implementation(variantOf(libs.jna) { artifactType("aar") })
    testImplementation(libs.junit)
    // JVM 单元测试：宿主机的 libjnidispatch 在 jar 里（aar 只含 Android ABI）。
    testImplementation(libs.jna)
    testImplementation(libs.kotlinx.coroutines.test)
}
