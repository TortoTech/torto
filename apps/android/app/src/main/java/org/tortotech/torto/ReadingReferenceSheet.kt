package org.tortotech.torto

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.testTag
import androidx.compose.ui.semantics.paneTitle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import org.tortotech.torto.core.referenceColor

/** Desktop-style marker gutter and shared rich text, adapted to a touch sheet. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable internal fun ReadingReferenceSheet(interaction:ReaderInteraction,fontSize:Float,dark:Boolean,model:ReaderModel){
    val note=interaction.kind=="note"
    val ink=Color(referenceColor(dark).toInt())
    val pane=tr("脚注内容","Footnote content")
    ModalBottomSheet(onDismissRequest=model::dismissInteraction,sheetState=rememberModalBottomSheetState(skipPartiallyExpanded=true),containerColor=MaterialTheme.colorScheme.surfaceContainerLow){
        Column(Modifier.fillMaxWidth().padding(horizontal=20.dp).navigationBarsPadding().padding(bottom=12.dp),verticalArrangement=Arrangement.spacedBy(12.dp)){
            if(!note){
                Row(Modifier.fillMaxWidth(),verticalAlignment=Alignment.CenterVertically){
                    Text(interaction.text,Modifier.weight(1f),style=MaterialTheme.typography.titleMedium,fontWeight=FontWeight.SemiBold)
                    ActionIcon("close",tr("关闭引用","Close reference")){model.dismissInteraction()}
                }
                HorizontalDivider(color=MaterialTheme.colorScheme.outlineVariant.copy(alpha=.45f),thickness=.5.dp)
            }
            Row(Modifier.fillMaxWidth().semantics {testTag="reference-note-body";if(note)paneTitle=pane},verticalAlignment=Alignment.Top){
                if(note&&interaction.marker.isNotBlank()){
                    val size=(fontSize*.8f).coerceAtLeast(14f)/LocalDensity.current.fontScale
                    Text(interaction.marker,Modifier.widthIn(min=28.dp).padding(top=2.dp,end=8.dp),color=ink,fontSize=size.sp,lineHeight=(size*1.5f).sp,fontWeight=FontWeight.Medium)
                }
                Box(Modifier.weight(1f)){ReadingDocument(interaction,model)}
            }
        }
    }
}
