package org.tortotech.torto

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.size
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.*
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.drawscope.scale
import androidx.compose.ui.graphics.vector.PathParser
import android.app.Activity
import androidx.core.view.WindowCompat
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp

internal val ReaderMonospace=staticCompositionLocalOf {android.graphics.Typeface.MONOSPACE}
private val Chinese = staticCompositionLocalOf { true }
@Composable fun tr(chinese: String, english: String) = if (Chinese.current) chinese else english
@Composable fun TortoTheme(state: ScreenState, content: @Composable () -> Unit) {
    val dark = if (state.active != null) state.dark else when(state.appTheme) { "dark" -> true; "light" -> false; else -> isSystemInDarkTheme() }
    val view = LocalView.current
    SideEffect {
        (view.context as? Activity)?.window?.let { window ->
            WindowCompat.getInsetsController(window,view).apply {
                isAppearanceLightStatusBars = !dark
                isAppearanceLightNavigationBars = !dark
            }
        }
    }
    val chinese = when(state.language) { "zh-CN" -> true; "en" -> false; else -> LocalConfiguration.current.locales[0].language == "zh" }
    val scheme = if(dark) darkColorScheme(
        primary=Color(0xFFDDC2A4),onPrimary=Color(0xFF402D18),primaryContainer=Color(0xFF57422D),onPrimaryContainer=Color(0xFFF4DFC6),
        secondary=Color(0xFFD0C3B4),onSecondary=Color(0xFF362F26),tertiary=Color(0xFFB8CEA5),surfaceTint=Color.Transparent,secondaryContainer=Color(0xFF4C443B),onSecondaryContainer=Color(0xFFEDE0D2),background=Color(0xFF121212),surface=Color(0xFF1C1C1C),
        onSurface=Color(0xFFE9E1D9),onBackground=Color(0xFFE9E1D9),surfaceContainer=Color(0xFF25221F),surfaceContainerLow=Color(0xFF211F1D),surfaceContainerHigh=Color(0xFF2C2925),surfaceContainerHighest=Color(0xFF37332E),onSurfaceVariant=Color(0xFFCEC4B8),outlineVariant=Color(0xFF4D463D)
    ) else lightColorScheme(
        primary=Color(0xFF7B6751),onPrimary=Color.White,primaryContainer=Color(0xFFF1DEC6),onPrimaryContainer=Color(0xFF2B1C0D),
        secondary=Color(0xFF6D6257),onSecondary=Color.White,tertiary=Color(0xFF566447),secondaryContainer=Color(0xFFEBDDCB),onSecondaryContainer=Color(0xFF342A1F),background=Color(0xFFFAF8F3),surface=Color(0xFFFAF8F3),
        onSurface=Color(0xFF211B16),onBackground=Color(0xFF211B16),surfaceContainer=Color(0xFFF1ECE4),surfaceContainerLow=Color(0xFFF7F2EB),surfaceContainerHigh=Color(0xFFECE6DD),surfaceContainerHighest=Color(0xFFE6E0D6),onSurfaceVariant=Color(0xFF6D6257),outlineVariant=Color(0xFFD4C9BD),surfaceTint=Color.Transparent
    )
    CompositionLocalProvider(Chinese provides chinese,ReaderMonospace provides state.monoTypeface) { MaterialTheme(colorScheme=scheme,content=content) }
}

/** Small outlined symbols matching the old app's Material toolbar roles. */
private val symbolPaths = mapOf(
    "back" to "M20 12H4 M11 5L4 12L11 19", "previous" to "M15 5L8 12L15 19", "next" to "M9 5L16 12L9 19", "add" to "M12 4V20 M4 12H20",
    "close" to "M5 5L19 19 M19 5L5 19", "book" to "M4 3H19V21H6Q3 21 3 18V6Q3 3 6 3 M4 17H19 M7 7H15 M7 10H15",
    "library" to "M5 3V21 M9 3V21 M13 3V21 M17 3L21 20",
    "person" to "M16 7A4 4 0 1 1 8 7A4 4 0 1 1 16 7 M4 21V19Q4 14 12 14Q20 14 20 19V21",
    "search" to "M16 10A6 6 0 1 1 4 10A6 6 0 1 1 16 10 M14.5 14.5L21 21",
    "chat" to "M4 3H20V17H9L3 21V4Q3 3 4 3 M7 7H17 M7 11H14",
    "toc" to "M8 5H21 M8 12H21 M8 19H21 M3 5H4 M3 12H4 M3 19H4",
    "marks" to "M6 3H18V21L12 17L6 21Z", "note" to "M4 3H15L20 8V21H4Z M15 3V8H20 M8 12H16 M8 16H14",
    "translate" to "M2 5H14 M8 2V5 M4 7Q6 13 13 16 M12 5Q11 12 3 16 M14 21L18 11L22 21 M16 18H20",
    "select" to "M8 3H3V8 M16 3H21V8 M3 16V21H8 M16 21H21V16 M8 8H16V16H8Z",
    "type" to "M3 6H15 M9 6V21 M6 21H12 M15 15H23 M19 15V21",
    "moon" to "M20 15A9 9 0 0 1 9 3A9 9 0 1 0 20 15Z",
    "sun" to "M16 12A4 4 0 1 1 8 12A4 4 0 1 1 16 12 M12 1V4 M12 20V23 M1 12H4 M20 12H23 M4 4L6 6 M18 18L20 20 M4 20L6 18 M18 6L20 4",
    "sync" to "M3 10A9 9 0 0 1 19 6L22 9 M22 3V9H16 M21 14A9 9 0 0 1 5 18L2 15 M2 21V15H8",
    "settings" to "M4 7H20 M4 17H20 M8 4V10 M16 14V20",
    "cloud" to "M6 18A5 5 0 0 1 5 8A7 7 0 0 1 18 8A5 5 0 0 1 19 18Z",
    "ai" to "M12 2L15 9L22 12L15 15L12 22L9 15L2 12L9 9Z",
    "stats" to "M4 20H21 M6 17V11 M12 17V4 M18 17V8",
    "info" to "M21 12A9 9 0 1 1 3 12A9 9 0 1 1 21 12 M12 11V17 M12 7V8",
    "delete" to "M3 6H21 M9 6V3H15V6 M6 6L7 21H17L18 6 M10 10V17 M14 10V17",
    "image" to "M3 3H21V21H3Z M3 17L9 11L14 16L17 13L21 17 M15 7H16",
    "send" to "M3 3L22 12L3 21L6 12Z M6 12H22", "stop" to "M5 5H19V19H5Z",
    "copy" to "M8 8H21V21H8Z M16 8V3H3V16H8", "edit" to "M4 16L16 4L20 8L8 20H4Z M14 6L18 10",
    "check" to "M4 12L9 17L20 6", "undo" to "M8 4L3 9L8 14 M3 9H14A6 6 0 0 1 14 21H12",
)
@Composable fun AppIcon(name: String, description: String? = null, modifier: Modifier = Modifier, tint: Color = LocalContentColor.current) {
    val path=remember(name) { PathParser().parsePathString(symbolPaths[name] ?: symbolPaths.getValue("info")).toPath() }
    Canvas(modifier.size(24.dp).then(if(description==null) Modifier else Modifier.semantics { contentDescription=description })) {
        scale(size.width/24,size.height/24,pivot=androidx.compose.ui.geometry.Offset.Zero) { drawPath(path,tint,style=Stroke(width=1.7f,cap=StrokeCap.Round,join=StrokeJoin.Round)) }
    }
}
@Composable fun ActionIcon(name: String,label: String,enabled: Boolean=true,onClick:()->Unit) {
    IconButton(onClick=onClick,enabled=enabled) { AppIcon(name,label) }
}
