package org.tortotech.torto

import android.graphics.drawable.ColorDrawable
import android.view.WindowManager
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.*
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.platform.*
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.compose.ui.window.DialogWindowProvider
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import kotlinx.coroutines.flow.first

/** Focus once per opening, after this window can actually accept IME input. */
@Composable internal fun rememberInputFocus(enabled:Boolean=true):FocusRequester {
    val requester=remember {FocusRequester()}
    val window=LocalWindowInfo.current
    val keyboard=LocalSoftwareKeyboardController.current
    LaunchedEffect(enabled){
        if(enabled){
            snapshotFlow {window.isWindowFocused}.first {it}
            withFrameNanos {}
            requester.requestFocus()
            keyboard?.show()
        }
    }
    return requester
}

/** Shared full-screen input window, with stable geometry while the IME animates. */
@Composable internal fun FullScreenInputDialog(close:()->Unit,brightness:Float=-1f,content:@Composable (()->Unit)->Unit){
    val keyboard=LocalSoftwareKeyboardController.current
    val focus=LocalFocusManager.current
    val dismiss={focus.clearFocus();keyboard?.hide();close()}
    Dialog(onDismissRequest=dismiss,properties=DialogProperties(usePlatformDefaultWidth=false,decorFitsSystemWindows=false,dismissOnClickOutside=false)){
        val view=LocalView.current
        val window=(view.parent as? DialogWindowProvider)?.window
        val dark=MaterialTheme.colorScheme.background.luminance()<.5f
        ReadingWindowBrightness(window,true,brightness)
        DisposableEffect(window){window?.let {
            it.setBackgroundDrawable(ColorDrawable(android.graphics.Color.TRANSPARENT))
            it.setDimAmount(0f)
            it.setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_ADJUST_NOTHING)
            it.setWindowAnimations(R.style.Animation_Torto_ImagePreview)
        };onDispose {}}
        val bars=remember(window,view){window?.let {WindowCompat.getInsetsController(it,view)}}
        LaunchedEffect(bars){bars?.show(WindowInsetsCompat.Type.systemBars())}
        SideEffect {
            if(bars?.isAppearanceLightStatusBars==dark)bars.isAppearanceLightStatusBars=!dark
            if(bars?.isAppearanceLightNavigationBars==dark)bars.isAppearanceLightNavigationBars=!dark
        }
        content(dismiss)
    }
}
