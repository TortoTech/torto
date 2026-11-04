package org.tortotech.torto

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.ui.Alignment
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.Dp
import androidx.compose.foundation.interaction.collectIsPressedAsState
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.size
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.*
import android.app.Activity
import androidx.core.view.WindowCompat
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.selected
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

/** Individually bundled, optically sized Material Symbols Rounded vectors. */
private val symbolResources=mapOf(
    "back" to R.drawable.ic_symbol_back,
    "previous" to R.drawable.ic_symbol_previous,
    "next" to R.drawable.ic_symbol_next,
    "add" to R.drawable.ic_symbol_add,
    "close" to R.drawable.ic_symbol_close,
    "book" to R.drawable.ic_symbol_book,
    "library" to R.drawable.ic_symbol_library,
    "person" to R.drawable.ic_symbol_person,
    "search" to R.drawable.ic_symbol_search,
    "chat" to R.drawable.ic_symbol_chat,
    "toc" to R.drawable.ic_symbol_toc,
    "marks" to R.drawable.ic_symbol_marks,
    "note" to R.drawable.ic_symbol_note,
    "translate" to R.drawable.ic_symbol_translate,
    "sentences" to R.drawable.ic_symbol_sentences,
    "select" to R.drawable.ic_symbol_select,
    "type" to R.drawable.ic_symbol_type,
    "moon" to R.drawable.ic_symbol_moon,
    "sun" to R.drawable.ic_symbol_sun,
    "sync" to R.drawable.ic_symbol_sync,
    "settings" to R.drawable.ic_symbol_settings,
    "cloud" to R.drawable.ic_symbol_cloud,
    "ai" to R.drawable.ic_symbol_ai,
    "stats" to R.drawable.ic_symbol_stats,
    "info" to R.drawable.ic_symbol_info,
    "delete" to R.drawable.ic_symbol_delete,
    "image" to R.drawable.ic_symbol_image,
    "send" to R.drawable.ic_symbol_send,
    "stop" to R.drawable.ic_symbol_stop,
    "copy" to R.drawable.ic_symbol_copy,
    "edit" to R.drawable.ic_symbol_edit,
    "check" to R.drawable.ic_symbol_check,
    "undo" to R.drawable.ic_symbol_undo,
    "expand" to R.drawable.ic_symbol_expand
)
private val filledSymbolResources=mapOf(
    "library" to R.drawable.ic_symbol_library_filled,
    "marks" to R.drawable.ic_symbol_marks_filled,
    "person" to R.drawable.ic_symbol_person_filled,
    "toc" to R.drawable.ic_symbol_toc_filled
)
@Composable fun AppIcon(name:String,description:String?=null,modifier:Modifier=Modifier,tint:Color=LocalContentColor.current,filled:Boolean=false,size:Dp=24.dp) {
    val resource=(if(filled)filledSymbolResources[name] else null) ?: symbolResources[name] ?: R.drawable.ic_symbol_info
    Icon(androidx.compose.ui.res.painterResource(resource),description,modifier.size(size*1.2f),tint=tint)
}
/** A transparent 48dp target; only the unchanged icon's ink reacts to input. */
@Composable fun ActionIcon(name:String,label:String,enabled:Boolean=true,selected:Boolean=false,modifier:Modifier=Modifier,onClick:()->Unit) {
    val interaction=remember {androidx.compose.foundation.interaction.MutableInteractionSource()}
    val pressed by interaction.collectIsPressedAsState()
    val target=when { !enabled->MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha=.38f);selected||pressed->MaterialTheme.colorScheme.primary;else->MaterialTheme.colorScheme.onSurfaceVariant }
    val ink by androidx.compose.animation.animateColorAsState(target,androidx.compose.animation.core.tween(100),label="icon ink")
    Box(modifier.size(48.dp).semantics {this.selected=selected}
        .clickable(interactionSource=interaction,indication=null,enabled=enabled,role=Role.Button,onClick=onClick),contentAlignment=Alignment.Center){
        AppIcon(name,label,size=22.dp,tint=ink)
    }
}
