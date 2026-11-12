package org.tortotech.torto

import androidx.compose.foundation.*
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.text
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.foundation.text.InlineTextContent
import androidx.compose.foundation.text.appendInlineContent
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.Placeholder
import androidx.compose.ui.text.PlaceholderVerticalAlign
import androidx.compose.ui.unit.em
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import org.tortotech.torto.core.Annotation

@Composable internal fun AnnotationsPanel(state:ScreenState,model:ReaderModel,search:()->Unit,close:()->Unit){
    val book=state.active
    val cover by produceState(book?.let {model.cachedCover(it.id)},book?.id){value=book?.let {model.cover(it.id)}}
    Column(Modifier.fillMaxSize()){
        Text(tr("笔记","Notes"),Modifier.fillMaxWidth().padding(vertical=12.dp),fontSize=16.sp,textAlign=TextAlign.Center)
        LazyColumn(Modifier.weight(1f),contentPadding=PaddingValues(16.dp),verticalArrangement=Arrangement.spacedBy(12.dp)){
            item("summary"){
                SettingsGroup {
                    Row(Modifier.fillMaxWidth().padding(16.dp),verticalAlignment=Alignment.CenterVertically){
                        Column(Modifier.weight(1f)){
                            Text(book?.title.orEmpty(),fontSize=18.sp,fontWeight=FontWeight.SemiBold,maxLines=2,overflow=TextOverflow.Ellipsis)
                            Spacer(Modifier.height(10.dp))
                            Text(tr("共 ${state.annotations.size} 条笔记","${state.annotations.size} "+if(state.annotations.size==1)"note"else"notes"),fontSize=14.sp,color=MaterialTheme.colorScheme.onSurfaceVariant)
                        }
                        cover?.let {bitmap->
                            Spacer(Modifier.width(16.dp))
                            Image(bitmap.asImageBitmap(),null,Modifier.size(54.dp,76.dp),contentScale=ContentScale.Fit)
                        }
                    }
                    HorizontalDivider(Modifier.padding(horizontal=16.dp),thickness=.5.dp,color=MaterialTheme.colorScheme.outlineVariant.copy(alpha=.4f))
                    SettingsItem(tr("搜索笔记","Search notes"),icon="search",onClick=search)
                }
            }
            noteEntries(state.annotations){model.jumpAnnotation(it,close)}
        }
    }
}

@Composable internal fun NotesSearchPanel(state:ScreenState,model:ReaderModel,cancel:()->Unit,close:()->Unit){
    var query by rememberSaveable(state.active?.id){mutableStateOf("")}
    val term=query.trim()
    val results=remember(state.annotations,term){if(term.isEmpty())emptyList()else state.annotations.filter {it.quote.contains(term,true)||it.note.orEmpty().contains(term,true)}}
    val focus=rememberInputFocus()
    val keyboard=LocalSoftwareKeyboardController.current
    val manager=LocalFocusManager.current
    val height=with(LocalDensity.current){maxOf(40.dp,24.sp.toDp()+8.dp)}
    DisposableEffect(Unit){onDispose {manager.clearFocus();keyboard?.hide()}}
    Column(Modifier.fillMaxSize()){
        Box(Modifier.padding(horizontal=16.dp).fillMaxWidth().height(height+8.dp),contentAlignment=Alignment.Center){
            Row(Modifier.fillMaxWidth().height(height).background(MaterialTheme.colorScheme.surfaceContainerHigh,RoundedCornerShape(20.dp)).padding(3.dp),verticalAlignment=Alignment.CenterVertically){
                ReaderSearchInput(query,{query=it},tr("搜索笔记","Search notes"),focus,submit={manager.clearFocus();keyboard?.hide()},cancel=cancel)
            }
        }
        if(term.isNotEmpty()){
            Text(tr("找到 ${results.size} 条笔记","${results.size} "+(if(results.size==1)"note"else"notes")+" found"),Modifier.padding(horizontal=32.dp,vertical=8.dp),style=MaterialTheme.typography.labelMedium,color=MaterialTheme.colorScheme.onSurfaceVariant)
            LazyColumn(Modifier.weight(1f),contentPadding=PaddingValues(16.dp),verticalArrangement=Arrangement.spacedBy(12.dp)){
                noteEntries(results){manager.clearFocus();keyboard?.hide();model.jumpAnnotation(it,close)}
            }
        }else Spacer(Modifier.weight(1f))
    }
}

private fun LazyListScope.noteEntries(notes:List<Annotation>,jump:(Annotation)->Unit){
    // Native metadata supplies repaired TOC identities and source ordering.
    notes.groupBy {it.chapterId}.forEach {(id,chapter)->
        item("chapter:$id"){
            val note=chapter.first()
            Text(note.chapterTitle.ifBlank {
                if(note.sectionIndex==UInt.MAX_VALUE)tr("未归类笔记","Other notes")
                else tr("第 ${note.sectionIndex.toLong()+1} 节","Section ${note.sectionIndex.toLong()+1}")
            },Modifier.padding(start=4.dp,top=12.dp,bottom=2.dp),fontSize=16.sp,fontWeight=FontWeight.Medium)
        }
        items(chapter,key={"note:${it.id}"}){note->NoteCard(note){jump(note)}}
    }
}

@Composable private fun NoteCard(note:Annotation,jump:()->Unit){
    val annotation=note.note?.takeIf {it.isNotBlank()}
    val label=if(annotation==null)tr("高亮","Highlight")else tr("批注","Note")
    Card(onClick=jump,modifier=Modifier.fillMaxWidth().semantics {contentDescription=label},shape=RoundedCornerShape(16.dp),colors=CardDefaults.cardColors(containerColor=MaterialTheme.colorScheme.surfaceContainerLowest)){
        Column(Modifier.fillMaxWidth().padding(16.dp)){
            val content=buildAnnotatedString {appendInlineContent("marker", "\uFFFC");append(annotation ?: note.quote)}
            val inline=mapOf("marker" to InlineTextContent(Placeholder(1.55.em,1.25.em,PlaceholderVerticalAlign.TextCenter)){
                Box(Modifier.fillMaxSize(),contentAlignment=Alignment.CenterStart){
                    AppIcon(if(annotation==null)"highlight"else"note",size=IconSizes.Small,tint=defaultIconColor())
                }
            })
            Text(content,modifier=Modifier.clearAndSetSemantics {text=AnnotatedString(annotation ?: note.quote)},fontSize=16.sp,maxLines=if(annotation==null)4 else Int.MAX_VALUE,overflow=TextOverflow.Ellipsis,inlineContent=inline)
            if(annotation!=null){
                Spacer(Modifier.height(12.dp))
                Row(Modifier.height(IntrinsicSize.Min)){
                    Box(Modifier.fillMaxHeight().width(2.dp).background(MaterialTheme.colorScheme.outlineVariant))
                    Spacer(Modifier.width(8.dp))
                    Text(note.quote,fontSize=14.sp,color=MaterialTheme.colorScheme.onSurfaceVariant,maxLines=4,overflow=TextOverflow.Ellipsis)
                }
            }
        }
    }
}
