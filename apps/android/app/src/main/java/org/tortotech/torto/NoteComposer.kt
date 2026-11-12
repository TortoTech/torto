package org.tortotech.torto

import androidx.compose.foundation.*
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.semantics.*
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/** Independent IME window: the book and reading position remain stationary. */
@Composable internal fun NoteComposerDialog(state:ScreenState,quote:String,initial:String?,close:()->Unit,save:(String,()->Unit)->Unit){
    var text by remember(quote,initial){mutableStateOf(initial.orEmpty())}
    var active by remember {mutableStateOf(true)}
    DisposableEffect(Unit){onDispose {active=false}}
    val title=tr("写想法","Write a note")
    val inputLabel=tr("批注内容","Note content")
    val sendLabel=tr("发送批注","Send note")
    val placeholder=tr("写下这一刻的想法…","Write down your thoughts…")
    val reference=tr("引用：","Quote: ")+remember(quote){quote.lineSequence().joinToString(" "){it.trim()}}
    FullScreenInputDialog(close,state.readingBrightness){dismiss->
        val focus=rememberInputFocus(!state.busy)
        Box(Modifier.fillMaxSize().background(Color.Black.copy(alpha=.35f)).semantics {paneTitle=title}.imePadding()){
            Box(Modifier.fillMaxSize().clickable(interactionSource=remember {MutableInteractionSource()},indication=null,onClickLabel=tr("关闭批注","Close note"),onClick=dismiss))
            Surface(Modifier.align(Alignment.BottomCenter).fillMaxWidth(),color=MaterialTheme.colorScheme.background){
                Column(Modifier.navigationBarsPadding().padding(horizontal=16.dp,vertical=14.dp),verticalArrangement=Arrangement.spacedBy(12.dp)){
                    Row(Modifier.fillMaxWidth(),verticalAlignment=Alignment.CenterVertically,horizontalArrangement=Arrangement.spacedBy(10.dp)){
                        Surface(Modifier.weight(1f),shape=RoundedCornerShape(24.dp),color=MaterialTheme.colorScheme.surfaceContainerLowest){
                            BasicTextField(text,{text=it},Modifier.fillMaxWidth().heightIn(min=42.dp,max=160.dp).padding(horizontal=14.dp,vertical=10.dp).focusRequester(focus).semantics {contentDescription=inputLabel},
                                enabled=!state.busy,maxLines=5,textStyle=MaterialTheme.typography.bodyLarge.copy(fontSize=16.sp,lineHeight=24.sp,color=MaterialTheme.colorScheme.onSurface),cursorBrush=SolidColor(brandControlColor()),
                                decorationBox={input->Box {if(text.isEmpty())Text(placeholder,fontSize=16.sp,color=MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha=.6f));input()}})
                        }
                        val enabled=text.isNotBlank()&&!state.busy
                        Box(Modifier.size(40.dp).clip(CircleShape).background(brandControlColor().copy(alpha=if(enabled)1f else .3f)).semantics {contentDescription=sendLabel}.clickable(enabled=enabled,role=Role.Button){save(text.trim()){if(active)dismiss()}},contentAlignment=Alignment.Center){
                            if(state.busy)CircularProgressIndicator(Modifier.size(18.dp),color=Color.White,strokeWidth=2.dp)else AppIcon("up",size=24.dp,tint=Color.White)
                        }
                    }
                    Text(reference,Modifier.fillMaxWidth(),fontSize=13.sp,lineHeight=20.sp,color=MaterialTheme.colorScheme.onSurfaceVariant,maxLines=1,overflow=TextOverflow.Ellipsis)
                }
            }
        }
    }
}
