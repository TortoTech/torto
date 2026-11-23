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
    val reader=remember {HomeTransitionSnapshot()}
    val colors=MaterialTheme.colorScheme
    if(state.active==null)SideEffect {home.state=state;home.colors=colors}
    else SideEffect {reader.state=state;reader.colors=colors}
    val transition=updateTransition(state.active!=null,label="open reader")
    LaunchedEffect(transition.currentState,transition.targetState){
        if(transition.currentState&&transition.targetState){home.state=null;home.colors=null}
        if(!transition.currentState&&!transition.targetState){reader.state=null;reader.colors=null}
    }
    transition.AnimatedContent(
        modifier=Modifier.fillMaxSize(),
        transitionSpec={
            if(targetState) ContentTransform(
                targetContentEnter=slideInHorizontally(tween(280)){it},
                initialContentExit=slideOutHorizontally(tween(280)){-it/3},
                targetContentZIndex=1f,
            ).using(null)
            else ContentTransform(
                targetContentEnter=slideInHorizontally(tween(280)){-it/3},
                initialContentExit=slideOutHorizontally(tween(280)){it},
                targetContentZIndex=0f,
            ).using(null)
        },
    ){reading->
        val exitingHome=!reading&&state.active!=null
        val exitingReader=reading&&state.active==null
        val displayed=when{
            exitingHome->home.state ?: state.copy(active=null,frame=null)
            exitingReader->reader.state ?: return@AnimatedContent
            else->state
        }
        val exiting=exitingHome||exitingReader
        val blocker=if(exiting)Modifier.clearAndSetSemantics {}.pointerInput(Unit){
            awaitPointerEventScope {while(true)awaitPointerEvent(PointerEventPass.Initial).changes.forEach {it.consume()}}
        }else Modifier
        val scheme=when{
            exitingHome->home.colors ?: colors
            exitingReader->reader.colors ?: colors
            else->colors
        }
        MaterialTheme(colorScheme=scheme){
            Box(Modifier.fillMaxSize().then(blocker)){content(displayed)}
        }
    }
}
