package org.tortotech.torto

import androidx.compose.animation.*
import androidx.compose.animation.core.tween
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.delay

/** A terminal failure is displayed once, after Rust's shared retry budget.
 * Hiding the notice never clears failure caching or resubmits the request. */
@Composable internal fun ReaderAiNotice(state:ScreenState,modifier:Modifier=Modifier){
    var visible by remember(state.active?.id){mutableStateOf(false)}
    var message by remember(state.active?.id){mutableStateOf("translation")}
    LaunchedEffect(state.active?.id,state.aiErrorEvent,state.aiError,state.aiNoticeUntil){
        val remaining=state.aiNoticeUntil-android.os.SystemClock.uptimeMillis()
        if(state.aiError!=null&&remaining>0){
            message=if(state.aiErrorKind=="translation")"translation"else"layout"
            visible=true
            delay(remaining)
            visible=false
        } else visible=false
    }
    AnimatedVisibility(visible,modifier=modifier,enter=fadeIn(tween(120)),exit=fadeOut(tween(120))){
        Surface(shape=RoundedCornerShape(10.dp),color=MaterialTheme.colorScheme.inverseSurface,
            contentColor=MaterialTheme.colorScheme.inverseOnSurface,shadowElevation=3.dp){
            Text(if(message=="translation")tr("翻译服务错误","Translation service error")else tr("AI 排版服务错误","AI layout service error"),
                Modifier.padding(horizontal=16.dp,vertical=10.dp),fontSize=14.sp)
        }
    }
}
