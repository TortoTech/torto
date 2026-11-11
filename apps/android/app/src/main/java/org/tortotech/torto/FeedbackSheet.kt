package org.tortotech.torto

import android.content.Intent
import android.graphics.BitmapFactory
import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.*
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.withContext
import org.json.JSONObject

@OptIn(ExperimentalMaterial3Api::class)
@Composable internal fun FeedbackSheet(state:FeedbackUiState,model:FeedbackModel){
    val context=LocalContext.current
    val picker=rememberLauncherForActivityResult(ActivityResultContracts.PickMultipleVisualMedia(3)){model.addImages(context,it)}
    val waiting by produceState(false,state.retryUntil){
        val remaining=state.retryUntil.toLong()-System.currentTimeMillis()
        value=remaining>0;if(remaining>0){delay(remaining);value=false}
    }
    val disabled=state.loading||state.working||state.sending||state.locked
    ModalBottomSheet(onDismissRequest=model::close,sheetState=rememberModalBottomSheetState(skipPartiallyExpanded=true),containerColor=MaterialTheme.colorScheme.surfaceContainerLow){
        Column(Modifier.fillMaxWidth().fillMaxHeight(.94f).navigationBarsPadding().imePadding()){
            Box(Modifier.fillMaxWidth().height(48.dp).padding(horizontal=8.dp)){
                Text(tr("反馈问题","Report an issue"),Modifier.align(Alignment.Center),fontSize=16.sp,textAlign=TextAlign.Center)
                ActionIcon("close",tr("关闭反馈","Close feedback"),modifier=Modifier.align(Alignment.CenterStart),onClick=model::close)
            }
            Column(Modifier.weight(1f).verticalScroll(rememberScrollState()).padding(horizontal=16.dp),verticalArrangement=Arrangement.spacedBy(16.dp)){
                if(state.loading)LinearProgressIndicator(Modifier.fillMaxWidth())
                if(state.status=="submitted"){
                    Text(tr("反馈已提交，感谢你的帮助","Feedback submitted. Thank you"),Modifier.padding(top=24.dp),style=MaterialTheme.typography.titleMedium)
                    state.issueUrl?.let {url->TextButton(onClick={context.startActivity(Intent(Intent.ACTION_VIEW,Uri.parse(url)))}){Text(tr("查看反馈","View issue")+" #${state.issueNumber}")}}
                }else{
                    Row(horizontalArrangement=Arrangement.spacedBy(12.dp)){
                        FilterChip(state.kind=="bug",{model.edit(kind="bug")},label={Text(tr("缺陷","Bug"))},enabled=!disabled)
                        FilterChip(state.kind=="feature",{model.edit(kind="feature")},label={Text(tr("功能","Feature"))},enabled=!disabled)
                    }
                    OutlinedTextField(state.description,{if(it.length<=10000)model.edit(description=it)},Modifier.fillMaxWidth().heightIn(min=180.dp),enabled=!disabled,
                        placeholder={Text(if(state.kind=="bug")tr("发生了什么？如何重现？你期望出现什么结果？","What happened? How can we reproduce it? What did you expect?")else tr("希望增加什么功能？它能解决什么问题？","What feature would help, and what problem would it solve?"))},
                        supportingText={Text(tr("至少 10 个非空白字符","At least 10 non-whitespace characters")+" · ${state.count}/5000",color=if(state.count>5000u)MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant)})
                    Row(Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()),horizontalArrangement=Arrangement.spacedBy(12.dp)){
                        state.images.forEach {image->key(image.index,image.thumbnail){
                            val bitmap by produceState<android.graphics.Bitmap?>(null,image.thumbnail){value=withContext(Dispatchers.Default){BitmapFactory.decodeByteArray(image.thumbnail,0,image.thumbnail.size)}}
                            Column(horizontalAlignment=Alignment.CenterHorizontally){
                                Box(Modifier.size(96.dp,76.dp).clickable(onClickLabel=tr("预览截图","Preview screenshot")){model.preview(image.index)}){
                                    bitmap?.let {Image(it.asImageBitmap(),tr("截图 ${image.index+1u}","Screenshot ${image.index+1u}"),Modifier.fillMaxSize(),contentScale=ContentScale.Fit)}
                                }
                                if(!state.locked)TextButton({model.removeImage(image.index)},enabled=!disabled){Text(tr("移除","Remove"))}
                            }
                        }}
                        if(state.images.size<3&&!state.locked)OutlinedButton(onClick={picker.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly))},enabled=!disabled){AppIcon("image",size=IconSizes.Compact);Spacer(Modifier.width(8.dp));Text(tr("添加截图","Add screenshots"))}
                    }
                    if(state.working)LinearProgressIndicator(Modifier.fillMaxWidth())
                    var expanded by rememberSaveable {mutableStateOf(false)}
                    SettingsGroup {
                        SettingsItem(tr("附带信息","Included information"),value=if(expanded)tr("收起","Hide")else tr("展开","Show")){expanded=!expanded}
                        if(expanded)FeedbackContext(state.context)
                    }
                }
                Spacer(Modifier.height(8.dp))
            }
            HorizontalDivider(thickness=.5.dp,color=MaterialTheme.colorScheme.outlineVariant)
            Column(Modifier.fillMaxWidth().background(MaterialTheme.colorScheme.surface).padding(16.dp),verticalArrangement=Arrangement.spacedBy(8.dp)){
                if(state.status=="pending")Text(tr("暂时无法确认提交结果，内容已保留，请稍后重试","The result is not yet confirmed. Your report is preserved; retry later"),fontSize=13.sp,color=MaterialTheme.colorScheme.onSurfaceVariant)
                state.error?.let {Text(feedbackError(it),fontSize=13.sp,color=MaterialTheme.colorScheme.error)}
                if(waiting)Text(tr("请稍后重试","Please retry later"),fontSize=13.sp,color=MaterialTheme.colorScheme.onSurfaceVariant)
                Button(onClick=if(state.status=="submitted")model::close else model::submit,
                    enabled=state.status=="submitted"||!state.loading&&!state.working&&!state.sending&&!waiting&&state.valid,modifier=Modifier.fillMaxWidth()){
                    if(state.sending){CircularProgressIndicator(Modifier.size(18.dp),strokeWidth=2.dp,color=MaterialTheme.colorScheme.onPrimary);Spacer(Modifier.width(8.dp))}
                    Text(if(state.status=="submitted")tr("完成","Done")else if(state.sending)tr("提交中","Submitting")else if(state.locked)tr("重试","Retry")else tr("提交","Submit"))
                }
            }
        }
    }
    state.preview?.let {FullScreenImagePreview(it,model::closePreview)}
}

@Composable private fun FeedbackContext(value:String){
    val context=remember(value){runCatching {JSONObject(value)}.getOrDefault(JSONObject())}
    val book=context.optJSONObject("book");val location=context.optJSONObject("location");val reader=context.optJSONObject("reader");val translation=reader?.optJSONObject("translation")
    val focus=location?.optJSONObject("focus")
    fun array(value:org.json.JSONArray?)=if(value==null)""else (0 until value.length()).joinToString(", "){value.optString(it)}
    val os=context.optJSONObject("os");val app=context.optJSONObject("app");val display=context.optJSONObject("display")
    val rows=listOfNotNull(
        tr("来源","Source") to if(context.optString("source")=="reader")tr("阅读页","Reader")else tr("应用页面","Application"),
        book?.let {tr("书籍","Book") to it.optString("title")},
        book?.let {array(it.optJSONArray("authors"))}?.takeIf {it.isNotBlank()}?.let {tr("作者","Authors") to it},
        book?.let {array(it.optJSONArray("languages"))}?.takeIf {it.isNotBlank()}?.let {tr("原书语言","Book language") to it},
        book?.let {tr("格式","Format") to it.optString("format")},
        book?.let {tr("原书布局","Book layout") to if(it.optString("layout")=="fixed")tr("固定布局","Fixed")else tr("可重排","Reflowable")},
        location?.optString("chapter")?.takeIf {it.isNotBlank()}?.let {tr("章节","Chapter") to it},
        location?.optString("document")?.takeIf {it.isNotBlank()}?.let {tr("书内文档","Book document") to it},
        location?.optJSONObject("focus")?.optJSONObject("range")?.optJSONObject("start")?.optString("node")?.let {tr("当前聚焦块","Focused block") to it},
        location?.optString("anchor")?.takeIf {it.isNotBlank()}?.let {tr("章节锚点","Chapter anchor") to it},
        location?.takeIf {it.has("page")}?.let {tr("PDF 页码","PDF page") to it.optInt("page").toString()},
        reader?.let {tr("阅读模式","Reading mode") to if(it.optString("mode")=="focus")tr("专注模式","Focus mode")else tr("经典模式","Classic mode")},
        reader?.let {tr("排版","Typesetting") to if(it.optString("typesetting")=="unified")tr("统一版式","Unified typesetting")else tr("原书版式","Original typesetting")},
        reader?.optString("pdfReflow")?.takeIf {it.isNotBlank()}?.let {tr("PDF 视图","PDF view") to when(it){"local"->tr("本地重排","Local reflow");"ai"->tr("AI 重排","AI reflow");else->tr("原版","Original")}},
        translation?.let {tr("翻译","Translation") to if(it.optBoolean("enabled"))tr("已开启","On")+" · "+it.optString("targetLanguage")else tr("已关闭","Off")},
        tr("应用版本","App version") to app?.optString("version").orEmpty(),tr("设备","Device") to os?.optString("version").orEmpty(),
        tr("系统架构","Architecture") to os?.optString("arch").orEmpty(),tr("屏幕尺寸","Display") to "${display?.optInt("width")} × ${display?.optInt("height")} px",
        tr("显示缩放","Display scale") to "${((display?.optDouble("scale") ?: 1.0)*100).toInt()}%"
    )
    Column(Modifier.padding(horizontal=16.dp,vertical=8.dp),verticalArrangement=Arrangement.spacedBy(10.dp)){
        rows.forEach {(label,text)->Row(Modifier.fillMaxWidth(),horizontalArrangement=Arrangement.spacedBy(12.dp)){
            Text(label,Modifier.width(88.dp),fontSize=13.sp,color=MaterialTheme.colorScheme.onSurfaceVariant)
            Text(text,Modifier.weight(1f),fontSize=13.sp)
        }}
        if(focus!=null){
            fun range(value:JSONObject?):String {
                fun anchor(v:JSONObject?)="${v?.optString("spine")} / ${v?.optString("node")} : ${v?.optLong("textOffset")}"
                return anchor(value?.optJSONObject("start"))+" → "+anchor(value?.optJSONObject("end"))
            }
            Text(tr("聚焦范围","Focus range"),fontSize=13.sp,color=MaterialTheme.colorScheme.onSurfaceVariant)
            androidx.compose.foundation.text.selection.SelectionContainer {Text(range(focus.optJSONObject("range")),fontSize=13.sp)}
            var members by rememberSaveable {mutableStateOf(false)}
            val ranges=focus.optJSONArray("ranges")
            TextButton(onClick={members=!members}){Text(tr("成员范围","Member ranges")+" · ${ranges?.length() ?: 0}/${focus.optInt("totalRanges")}")}
            if(members&&ranges!=null)androidx.compose.foundation.text.selection.SelectionContainer {Column(verticalArrangement=Arrangement.spacedBy(8.dp)){
                for(i in 0 until ranges.length())Text("${i+1}. "+range(ranges.optJSONObject(i)),fontSize=12.sp)
            }}
        }
    }
}
@Composable private fun feedbackError(code:String)=when(code){
    "network"->tr("无法连接反馈服务，内容已保留，请检查网络后重试","Cannot reach the feedback service. Your report is preserved; check your connection and retry")
    "rate_limited"->tr("提交过于频繁，请稍后重试","Too many submissions. Please retry later")
    "invalid"->tr("反馈内容不符合要求，请调整后重新提交","Please correct your report before submitting again")
    "image"->tr("无法读取图片，请选择有效且不超过 20 MiB 的图片","Choose a valid image no larger than 20 MiB")
    "limit"->tr("最多添加三张截图","Attach up to three screenshots")
    "storage"->tr("无法保存或读取反馈草稿，请重试","Could not save or restore the draft. Please retry")
    "configuration"->tr("反馈服务地址配置无效","The feedback service address is invalid")
    "conflict"->tr("反馈编号冲突，内容已保留，请联系维护者","Submission ID conflict. Your report is preserved; contact the maintainer")
    "protocol"->tr("暂时无法确认提交结果，内容已保留，请稍后核对","The result could not be verified. Your report is preserved; check again later")
    else->tr("反馈服务暂时不可用，内容已保留，请稍后重试","The feedback service is unavailable. Your report is preserved; retry later")
}
