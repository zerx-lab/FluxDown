pluginManagement {
    repositories {
        google {
            content {
                includeGroupByRegex("com\\.android.*")
                includeGroupByRegex("com\\.google.*")
                includeGroupByRegex("androidx.*")
            }
        }
        mavenCentral {
            if (providers.gradleProperty("fluxdown.ciMavenCentralMirror").orNull == "true") {
                url = uri("https://maven-central.storage-download.googleapis.com/maven2")
            }
        }
        gradlePluginPortal()
    }
}
plugins {
    id("org.gradle.toolchains.foojay-resolver-convention") version "1.0.0"
}
dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral {
            if (providers.gradleProperty("fluxdown.ciMavenCentralMirror").orNull == "true") {
                url = uri("https://maven-central.storage-download.googleapis.com/maven2")
            }
        }
    }
}

rootProject.name = "FluxDown"
include(":app", ":fluxui", ":core", ":bridge")
