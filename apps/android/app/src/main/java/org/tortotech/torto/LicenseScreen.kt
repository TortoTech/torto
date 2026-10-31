package org.tortotech.torto
import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONObject

private data class LicenseEntry(val name:String,val version:String,val license:String,val url:String,val text:String)
@Composable fun LicenseScreen(){
    val context=LocalContext.current
    val entries by produceState<List<LicenseEntry>?>(null){value=withContext(Dispatchers.IO){
        context.assets.open("open-source.json").bufferedReader().use {reader->val root=JSONObject(reader.readText());val packages=root.getJSONArray("packages");val texts=root.getJSONObject("texts")
            List(packages.length()){i->val p=packages.getJSONObject(i);val ids=p.getJSONArray("texts");LicenseEntry(p.getString("name"),p.getString("version"),p.getString("license"),p.getString("url"),List(ids.length()){texts.optString(ids.getString(it))}.joinToString("\n\n"))}
        }
    }}
    var selected by remember{mutableStateOf<LicenseEntry?>(null)};var query by remember{mutableStateOf("")}
    Column(Modifier.fillMaxSize().padding(20.dp)){
        Text(tr("Torto 使用以下开源组件","Torto uses the following open-source components"),style=MaterialTheme.typography.bodyMedium)
        Spacer(Modifier.height(12.dp));OutlinedTextField(query,{query=it},label={Text(tr("搜索组件或许可","Search components or licenses"))},modifier=Modifier.fillMaxWidth(),singleLine=true)
        if(entries==null)LinearProgressIndicator(Modifier.fillMaxWidth())
        LazyColumn(Modifier.weight(1f)){items(entries.orEmpty().filter {it.name.contains(query,true)||it.license.contains(query,true)}){entry->ListItem(headlineContent={Text(entry.name+" "+entry.version)},supportingContent={Text(entry.license)},modifier=Modifier.clickable {selected=entry},colors=ListItemDefaults.colors(containerColor=MaterialTheme.colorScheme.background))}}
    }
    selected?.let {entry->AlertDialog(onDismissRequest={selected=null},title={Text(entry.name)},text={Column(Modifier.heightIn(max=520.dp).verticalScroll(rememberScrollState())){Text(entry.license);Spacer(Modifier.height(12.dp));SelectionContainer{Text(entry.text.ifBlank {tr("上游许可信息请参阅项目链接","See the project link for upstream license information")},style=MaterialTheme.typography.bodySmall)}}},confirmButton={TextButton(onClick={selected=null}){Text(tr("关闭","Close"))}},dismissButton={if(entry.url.startsWith("https://")||entry.url.startsWith("http://"))TextButton(onClick={context.startActivity(Intent(Intent.ACTION_VIEW,Uri.parse(entry.url)))}){Text(tr("项目链接","Project link"))}})}
}
