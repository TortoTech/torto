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
internal val BrandGreen=Color(0xFF07C160)
@Composable internal fun brandControlColor()=if(MaterialTheme.colorScheme.background.luminance()>.5f)BrandGreen else MaterialTheme.colorScheme.primary
@Composable fun tr(chinese: String, english: String) = if (Chinese.current) chinese else english

// Vivid brand controls and accessible green text; surrounding surfaces stay neutral.
private val LightColors=lightColorScheme(
    primary=Color(0xFF078A46),onPrimary=Color.White,primaryContainer=Color(0xFFDEECE4),onPrimaryContainer=Color(0xFF193C2A),inversePrimary=Color(0xFF8AC5A2),
    secondary=Color(0xFF078A46),onSecondary=Color.White,secondaryContainer=Color(0xFFE7F1EB),onSecondaryContainer=Color(0xFF254632),
    tertiary=Color(0xFF078A46),onTertiary=Color.White,tertiaryContainer=Color(0xFFDEECE4),onTertiaryContainer=Color(0xFF193C2A),
    background=Color(0xFFF4F5F7),onBackground=Color(0xFF2B3039),surface=Color.White,onSurface=Color(0xFF2B3039),
    surfaceVariant=Color(0xFFEBEDF0),onSurfaceVariant=Color(0xFF7B828D),outline=Color(0xFFA3A9B2),outlineVariant=Color(0xFFDEE1E6),
    inverseSurface=Color(0xFF2B3037),inverseOnSurface=Color(0xFFF4F5F7),surfaceTint=Color.Transparent,scrim=Color.Black,
    error=Color(0xFFB43B36),onError=Color.White,errorContainer=Color(0xFFFBEAE8),onErrorContainer=Color(0xFF7F2824),
    surfaceBright=Color.White,surfaceDim=Color(0xFFE5E7EB),surfaceContainerLowest=Color.White,surfaceContainerLow=Color(0xFFF4F5F7),
    surfaceContainer=Color(0xFFF0F1F4),surfaceContainerHigh=Color(0xFFE9EBEF),surfaceContainerHighest=Color(0xFFE2E5E9),
    primaryFixed=Color(0xFFDEECE4),primaryFixedDim=Color(0xFFB7D6C4),onPrimaryFixed=Color(0xFF193C2A),onPrimaryFixedVariant=Color(0xFF315E44),
    secondaryFixed=Color(0xFFE7F1EB),secondaryFixedDim=Color(0xFFB7D6C4),onSecondaryFixed=Color(0xFF193C2A),onSecondaryFixedVariant=Color(0xFF315E44),
    tertiaryFixed=Color(0xFFDEECE4),tertiaryFixedDim=Color(0xFFB7D6C4),onTertiaryFixed=Color(0xFF193C2A),onTertiaryFixedVariant=Color(0xFF315E44),
)
private val DarkColors=darkColorScheme(
    primary=Color(0xFF36DA83),onPrimary=Color(0xFF10281B),primaryContainer=Color(0xFF263F31),onPrimaryContainer=Color(0xFFCCE6D6),inversePrimary=Color(0xFF078A46),
    secondary=Color(0xFF36DA83),onSecondary=Color(0xFF10281B),secondaryContainer=Color(0xFF293D32),onSecondaryContainer=Color(0xFFCCE6D6),
    tertiary=Color(0xFF36DA83),onTertiary=Color(0xFF10281B),tertiaryContainer=Color(0xFF263F31),onTertiaryContainer=Color(0xFFCCE6D6),
    background=Color(0xFF121416),onBackground=Color(0xFFE5E8EC),surface=Color(0xFF1C1F23),onSurface=Color(0xFFE5E8EC),
    surfaceVariant=Color(0xFF30363E),onSurfaceVariant=Color(0xFF9BA2AC),outline=Color(0xFF626B77),outlineVariant=Color(0xFF353C45),
    inverseSurface=Color(0xFFE5E8EC),inverseOnSurface=Color(0xFF252A31),surfaceTint=Color.Transparent,scrim=Color.Black,
    error=Color(0xFFE18D85),onError=Color(0xFF4A1714),errorContainer=Color(0xFF482725),onErrorContainer=Color(0xFFF3C3BD),
    surfaceBright=Color(0xFF383F48),surfaceDim=Color(0xFF121416),surfaceContainerLowest=Color(0xFF1C1F23),surfaceContainerLow=Color(0xFF121416),
    surfaceContainer=Color(0xFF22262C),surfaceContainerHigh=Color(0xFF2A3037),surfaceContainerHighest=Color(0xFF343B44),
    primaryFixed=Color(0xFFDEECE4),primaryFixedDim=Color(0xFFB7D6C4),onPrimaryFixed=Color(0xFF193C2A),onPrimaryFixedVariant=Color(0xFF315E44),
    secondaryFixed=Color(0xFFE7F1EB),secondaryFixedDim=Color(0xFFB7D6C4),onSecondaryFixed=Color(0xFF193C2A),onSecondaryFixedVariant=Color(0xFF315E44),
    tertiaryFixed=Color(0xFFDEECE4),tertiaryFixedDim=Color(0xFFB7D6C4),onTertiaryFixed=Color(0xFF193C2A),onTertiaryFixedVariant=Color(0xFF315E44),
)

@Composable internal fun tortoSwitchColors()=SwitchDefaults.colors(
    checkedTrackColor=BrandGreen,checkedThumbColor=Color.White,
    uncheckedTrackColor=MaterialTheme.colorScheme.surface,uncheckedThumbColor=MaterialTheme.colorScheme.outline,
    uncheckedBorderColor=MaterialTheme.colorScheme.outlineVariant,
)

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
    val scheme=if(dark)DarkColors else LightColors
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
    "feedback" to R.drawable.ic_symbol_feedback,
    "cloud" to R.drawable.ic_symbol_cloud,
    "ai" to R.drawable.ic_symbol_ai,
    "stats" to R.drawable.ic_symbol_stats,
    "info" to R.drawable.ic_symbol_info,
    "delete" to R.drawable.ic_symbol_delete,
    "image" to R.drawable.ic_symbol_image,
    "send" to R.drawable.ic_symbol_send,
    "stop" to R.drawable.ic_symbol_stop,
    "copy" to R.drawable.ic_symbol_copy,
    "highlight" to R.drawable.ic_symbol_highlight,
    "edit-note" to R.drawable.ic_symbol_edit_note,
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
    Box(modifier.size(size*1.2f)){
        Icon(androidx.compose.ui.res.painterResource(resource),description,Modifier.matchParentSize(),tint=tint)
        if(name=="highlight")androidx.compose.foundation.Canvas(Modifier.matchParentSize()){
            drawLine(BrandGreen,androidx.compose.ui.geometry.Offset(this.size.width*.125f,this.size.height*.9f),androidx.compose.ui.geometry.Offset(this.size.width*.875f,this.size.height*.9f),strokeWidth=this.size.width/10f,cap=StrokeCap.Round)
        }
    }
}
/** A transparent 48dp target; only the unchanged icon's ink reacts to input. */
@Composable fun ActionIcon(name:String,label:String,enabled:Boolean=true,selected:Boolean=false,modifier:Modifier=Modifier,iconSize:Dp=22.dp,onClick:()->Unit) {
    val interaction=remember {androidx.compose.foundation.interaction.MutableInteractionSource()}
    val pressed by interaction.collectIsPressedAsState()
    val target=when { !enabled->MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha=.38f);selected||pressed->brandControlColor();else->MaterialTheme.colorScheme.onSurfaceVariant }
    val ink by androidx.compose.animation.animateColorAsState(target,androidx.compose.animation.core.tween(100),label="icon ink")
    Box(modifier.size(48.dp).semantics {this.selected=selected}
        .clickable(interactionSource=interaction,indication=null,enabled=enabled,role=Role.Button,onClick=onClick),contentAlignment=Alignment.Center){
        AppIcon(name,label,size=iconSize,tint=ink)
    }
}
