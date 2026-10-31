package org.tortotech.torto

import android.graphics.Bitmap
import android.graphics.Typeface
import android.graphics.drawable.BitmapDrawable
import android.text.SpannableStringBuilder
import android.text.Spanned
import android.text.method.LinkMovementMethod
import android.text.style.*
import android.util.LruCache
import android.view.View
import android.widget.TextView
import androidx.compose.foundation.*
import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.conflate
import org.json.JSONArray
import org.tortotech.torto.core.chatMarkdown
import org.tortotech.torto.core.renderChatFormula

internal data class AssistantRun(val text:String,val style:Int,val link:String,val math:Boolean)
internal data class AssistantBlock(val kind:String,val level:Int,val prefix:String,val runs:List<AssistantRun>,val rows:List<List<List<AssistantRun>>>)
internal fun decodeAssistantMarkdown(json:String):List<AssistantBlock>{
    fun runs(array:JSONArray)=List(array.length()){i->array.getJSONObject(i).let {AssistantRun(it.getString("text"),it.getInt("style"),it.getString("link"),it.getBoolean("math"))}}
    val blocks=JSONArray(json)
    return List(blocks.length()){i->val b=blocks.getJSONObject(i);val rows=b.getJSONArray("rows");AssistantBlock(b.getString("kind"),b.getInt("level"),b.getString("prefix"),runs(b.getJSONArray("runs")),List(rows.length()){r->val cells=rows.getJSONArray(r);List(cells.length()){c->runs(cells.getJSONArray(c))}})}
}
/** Shared by the visible chat bubbles; no timers or network image loaders. */
internal class AssistantAssetCache {
    private val bitmaps=object:LruCache<String,Bitmap>(8*1024*1024){override fun sizeOf(key:String,value:Bitmap)=value.allocationByteCount}
    private val failures=linkedSetOf<String>()
    suspend fun formula(latex:String,size:Float,color:Int):Bitmap? {
        val key="$color:$size:$latex"
        bitmaps.get(key)?.let{return it}
        if(synchronized(failures){key in failures})return null
        return try {
            val bitmap=withContext(NonCancellable+Dispatchers.Default){
                renderChatFormula(latex,size,color.toUInt()).use {page->
                    Bitmap.createBitmap(page.width().toInt(),page.height().toInt(),Bitmap.Config.ARGB_8888).also {check(NativePixels.copyPage(page.id().toLong(),it))}
                }
            }
            bitmaps.put(key,bitmap);bitmap
        }catch(e:CancellationException){throw e}catch(_:Exception){synchronized(failures){failures.add(key);while(failures.size>64)failures.remove(failures.first())};null}
    }
}
private data class PreparedAssistant(val blocks:List<AssistantBlock>,val formulas:Map<String,Bitmap>)
@Composable internal fun AssistantRichText(text:String,cache:AssistantAssetCache,streaming:Boolean=false,onLink:(String)->Unit) {
    val context=LocalContext.current
    val foreground=LocalContentColor.current.toArgb()
    val accent=MaterialTheme.colorScheme.primary.toArgb()
    val size=16f*context.resources.displayMetrics.scaledDensity
    val currentText by rememberUpdatedState(text)
    val currentStreaming by rememberUpdatedState(streaming)
    val prepared by produceState<PreparedAssistant?>(null,foreground,size,cache){
        // Process the latest snapshot at a bounded cadence. Debouncing on each
        // token would postpone rich rendering until the stream became quiet.
        snapshotFlow {currentText to currentStreaming}.conflate().collect {
        if(currentStreaming)delay(120)
        val source=currentText
        try {
            val blocks=withContext(Dispatchers.Default){decodeAssistantMarkdown(chatMarkdown(source))}
            val math=blocks.flatMap {b->b.runs+b.rows.flatten().flatten()}.filter {it.math}.map {it.text}.distinct().take(24)
            val images=mutableMapOf<String,Bitmap>()
            var budget=8*1024*1024
            for(latex in math){ensureActive();if(budget<=0)break;cache.formula(latex,size.coerceIn(8f,144f),foreground)?.let {if(it.allocationByteCount<=budget){images[latex]=it;budget-=it.allocationByteCount}}}
            value=PreparedAssistant(blocks,images)
        }catch(e:CancellationException){throw e}catch(_:Exception){value=PreparedAssistant(listOf(AssistantBlock("paragraph",0,"",listOf(AssistantRun(source,0,"",false)),emptyList())),emptyMap())}
        }
    }
    val content=prepared
    if(content==null){Text(text);return}
    Column(verticalArrangement=Arrangement.spacedBy(10.dp)){
        for(block in content.blocks)when(block.kind){
            "rule"->HorizontalDivider()
            "table"->Column(Modifier.horizontalScroll(rememberScrollState()).border(1.dp,MaterialTheme.colorScheme.outlineVariant)){
                for((index,row)in block.rows.withIndex())Row(Modifier.background(if(index==0)MaterialTheme.colorScheme.surfaceContainerHighest else MaterialTheme.colorScheme.surface)){
                    for(cell in row)Box(Modifier.width(160.dp).border(.5.dp,MaterialTheme.colorScheme.outlineVariant).padding(10.dp)){
                        AssistantTextRuns(cell,content.formulas,foreground,accent,16f,index==0,false,onLink)
                    }
                }
            }
            else->{
                val code=block.kind=="code"
                Row(Modifier.padding(start=(block.level.coerceAtMost(6)*if(block.kind=="heading")0 else 12).dp).then(if(code||block.kind=="quote")Modifier.background(MaterialTheme.colorScheme.surfaceContainerHighest,MaterialTheme.shapes.small).padding(10.dp) else Modifier)){
                    if(block.prefix.isNotEmpty()&&!code)Text(block.prefix)
                    Column(Modifier.weight(1f)){
                        if(code&&block.prefix.isNotEmpty())Text(block.prefix,style=MaterialTheme.typography.labelSmall,color=MaterialTheme.colorScheme.onSurfaceVariant)
                        AssistantTextRuns(block.runs,content.formulas,foreground,accent,if(block.kind=="heading")when(block.level){1->24f;2->22f;else->18f} else 16f,block.kind=="heading",code,onLink)
                    }
                }
            }
        }
    }
}
@Composable private fun AssistantTextRuns(runs:List<AssistantRun>,formulas:Map<String,Bitmap>,foreground:Int,accent:Int,size:Float,bold:Boolean,code:Boolean,onLink:(String)->Unit){
    val latestLink by rememberUpdatedState(onLink)
    val mono=ReaderMonospace.current
    AndroidView(factory={TextView(it).apply {setTextIsSelectable(true);includeFontPadding=false;movementMethod=LinkMovementMethod.getInstance();setPadding(0,0,0,0)}},update={view->
        view.setTextColor(foreground);view.setLinkTextColor(accent);view.textSize=size
        view.typeface=if(code)mono else Typeface.create("sans-serif",if(bold)Typeface.BOLD else Typeface.NORMAL)
        val text=SpannableStringBuilder()
        for(run in runs){
            val start=text.length;text.append(run.text);val end=text.length
            fun span(value:Any){if(end>start)text.setSpan(value,start,end,Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)}
            if(run.style and 1!=0)span(StyleSpan(Typeface.BOLD))
            if(run.style and 2!=0)span(StyleSpan(Typeface.ITALIC))
            if(run.style and 4!=0)span(StrikethroughSpan())
            if(run.style and 8!=0)span(object:MetricAffectingSpan(){override fun updateDrawState(p:android.text.TextPaint){p.typeface=mono};override fun updateMeasureState(p:android.text.TextPaint){p.typeface=mono}})
            if(run.math)formulas[run.text]?.let {bitmap->
                val drawable=BitmapDrawable(view.resources,bitmap)
                drawable.setBounds(0,0,(bitmap.width/2).coerceAtLeast(1),(bitmap.height/2).coerceAtLeast(1))
                span(ImageSpan(drawable,if(android.os.Build.VERSION.SDK_INT>=29)ImageSpan.ALIGN_CENTER else ImageSpan.ALIGN_BASELINE))
            }
            if(run.link.isNotEmpty())span(object:ClickableSpan(){override fun onClick(widget:View){latestLink(run.link)};override fun updateDrawState(paint:android.text.TextPaint){paint.color=accent;paint.isUnderlineText=false}})
        }
        if(view.text!=text)view.text=text
    },modifier=Modifier.fillMaxWidth().wrapContentHeight())
}
