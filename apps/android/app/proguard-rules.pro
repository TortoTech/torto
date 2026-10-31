-keep class org.tortotech.torto.core.** { *; }
-keep class com.sun.jna.** { *; }
-keep class * implements com.sun.jna.Callback { *; }
-keep class org.tortotech.torto.NativePixels { *; }
-dontwarn java.awt.**

-keep class org.tortotech.torto.NativePlatform { *; }
-keep,includedescriptorclasses class org.rustls.platformverifier.** { *; }
