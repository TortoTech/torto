package org.tortotech.torto

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.testTag
import androidx.compose.ui.semantics.paneTitle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import org.tortotech.torto.core.referenceColor

/** Desktop-style first-line marker and shared rich text, adapted to a touch sheet. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable internal fun ReadingReferenceSheet(interaction:ReaderInteraction,fontSize:Float,dark:Boolean,model:ReaderModel,state:ScreenState){
    val note=interaction.kind=="note"
    val ink=Color(referenceColor(dark).toInt())
    val pane=tr("脚注内容","Footnote content")
    ModalBottomSheet(onDismissRequest=model::dismissInteraction,sheetState=rememberModalBottomSheetState(skipPartiallyExpanded=true),containerColor=MaterialTheme.colorScheme.surfaceContainerLow){
        Column(Modifier.fillMaxWidth().padding(horizontal=20.dp).navigationBarsPadding().padding(bottom=12.dp),verticalArrangement=Arrangement.spacedBy(12.dp)){
            val size=(fontSize*.8f).coerceAtLeast(14f)/LocalDensity.current.fontScale
            val style=TextStyle(fontSize=size.sp,lineHeight=(size*1.5f).sp,fontWeight=FontWeight.Medium)
            val measurer=rememberTextMeasurer()
            val density=LocalDensity.current
            val markerLayout=measurer.measure(AnnotatedString(interaction.marker),style)
            val markerWidth=if(note&&interaction.marker.isNotBlank())with(density){markerLayout.size.width.toDp()}else 0.dp
            var baseline by remember(interaction){mutableStateOf<Float?>(null)}
            val markerTop=(baseline?.minus(markerLayout.firstBaseline/density.density) ?: 0f).coerceAtLeast(0f)
            val indent=if(markerWidth>0.dp)markerWidth.value+with(density){(size*.3f).sp.toDp().value}else 0f
            Box(Modifier.fillMaxWidth().semantics {testTag="reference-note-body";if(note)paneTitle=pane}){
                ReadingDocument(interaction,model,firstLineIndent=indent,onBaseline={baseline=it})
                if(markerWidth>0.dp&&baseline!=null)Text(interaction.marker,Modifier.offset(y=markerTop.dp).padding(start=2.dp),color=ink,style=style)
                ReaderAiNotice(state,Modifier.align(Alignment.TopCenter))
            }
        }
    }
}
