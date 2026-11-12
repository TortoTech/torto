package org.tortotech.torto

import androidx.compose.animation.*
import androidx.compose.animation.core.tween
import androidx.compose.animation.core.updateTransition
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.ColorScheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.semantics.clearAndSetSemantics

private class HomeTransitionSnapshot { var state:ScreenState?=null;var colors:ColorScheme?=null }

/** Retain the existing home composition only for its exit; never copy screen pixels.
 * The reader mounts underneath immediately so opening and exit run concurrently. */
@Composable internal fun ReaderEntryHost(state:ScreenState,content:@Composable (ScreenState)->Unit){
    val home=remember {HomeTransitionSnapshot()}
    val colors=MaterialTheme.colorScheme
    if(state.active==null)SideEffect {home.state=state;home.colors=colors}
    val transition=updateTransition(state.active!=null,label="open reader")
    LaunchedEffect(transition.currentState,transition.targetState){
        if(transition.currentState&&transition.targetState){home.state=null;home.colors=null}
    }
    transition.AnimatedContent(
        modifier=Modifier.fillMaxSize(),
        transitionSpec={
            (EnterTransition.None togetherWith if(targetState)fadeOut(tween(60))else ExitTransition.None)
                .apply {targetContentZIndex=if(targetState)-1f else 0f}.using(null)
        },
    ){reading->
        // Returning has no exit animation. Never render a second shelf inside
        // the retiring reader slot while AnimatedContent disposes that slot.
        if(reading&&state.active==null)return@AnimatedContent
        val exiting=!reading&&state.active!=null
        val displayed=if(exiting)home.state ?: state.copy(active=null,frame=null)else state
        val blocker=if(exiting)Modifier.clearAndSetSemantics {}.pointerInput(Unit){
            awaitPointerEventScope {while(true)awaitPointerEvent(PointerEventPass.Initial).changes.forEach {it.consume()}}
        }else Modifier
        MaterialTheme(colorScheme=if(exiting)home.colors ?: colors else colors){
            Box(Modifier.fillMaxSize().then(blocker)){content(displayed)}
        }
    }
}
