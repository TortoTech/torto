package org.tortotech.torto

import android.graphics.Bitmap
import androidx.activity.compose.setContent
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import kotlinx.coroutines.runBlocking
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.tortotech.torto.core.*

@RunWith(AndroidJUnit4::class)
class ReadingIntegrationTest {
    @Test fun advancedFontsFocusSelectionAndProvidersRoundtrip():Unit=runBlocking {
        val context=InstrumentationRegistry.getInstrumentation().targetContext
        val core=MobileCore(File(context.filesDir,"core").absolutePath)
        val original=core.readingOptions()
        try {
            val reading=original.copy(fontWeight=575u,focusStyle="highlight",scrollFocus=true,sentenceSplit=true,translated=false)
            core.saveReadingOptions(reading)
            assertEquals(575u.toUShort(),core.readingOptions().fontWeight)
            assertTrue(core.fontFamilies().monospace.contains("monospace"))
            checkAdvancedFocus(core,reading)
            checkProviderSettings(core)
            core.saveReadingOptions(reading.copy(translated=true));assertTrue(core.readingOptions().translated)
        }finally{core.saveReadingOptions(original);core.close()}
    }
    private suspend fun checkProviderSettings(core:MobileCore){
        val original=core.aiOptions()
        var providerId:String?=null
        try {
            core.saveProvider(Provider("","custom","Device test provider","http://127.0.0.1:1/v1",listOf("test-model"),false),"test-only",false)
            val provider=core.providers().single {it.name=="Device test provider"};providerId=provider.id
            assertTrue(provider.keySaved)
            core.saveProvider(provider.copy(name="Device test renamed"),null,false)
            assertTrue(core.providers().single {it.id==provider.id}.keySaved)
            val ai=original.copy(layoutEnabled=false,layoutProvider=provider.id,layoutModel="test-model",layoutReasoning="none",translationProvider=provider.id,translationModel="test-model",translationReasoning="low",chatProvider=provider.id,chatModel="test-model",showOriginal=true,translateToc=true,expertTranslation=true)
            core.saveAiOptions(ai);assertEquals(ai,core.aiOptions())
        }finally{core.saveAiOptions(original);providerId?.let {core.deleteProvider(it)}}
    }
    private suspend fun checkAdvancedFocus(core:MobileCore,reading:ReadingOptions){
        val context=InstrumentationRegistry.getInstrumentation().targetContext
        val fixture=File(context.cacheDir,"advanced-reading.fb2")
        val paragraphs=(0..60).joinToString(""){ "<p>Paragraph $it: first sentence. A second sentence about reading and source selection.</p>" }
        fixture.writeText("""<?xml version="1.0" encoding="UTF-8"?><FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0"><description><title-info><book-title>字体与滚动专注测试</book-title><lang>en</lang></title-info></description><body><section><title><p>Settings</p></title>$paragraphs</section></body></FictionBook>""")
        val book=core.importBook(fixture.absolutePath).first {it.title=="字体与滚动专注测试"}
        val reader=core.openBook(book.id,Viewport(360u,640u,2f))
        try {
            reader.goToSource(reader.search("Paragraph 0:").single().source).close()
            reader.focusMode(true).close()
            val page=reader.scrollBy(450f)
            val info=try{page.info()}finally{page.close()}
            assertTrue(info.isScroll);assertTrue(info.focusHighlight);assertEquals(450.0,info.scrollDistance,0.1)
            checkNativeHandles(reader,info)
            checkViewGestures(info)
            reader.applyReadingOptions(reading.copy(fontWeight=650u,focusStyle="dim")).use {assertFalse(it.info().focusHighlight)}
            reader.focusMode(false).use {assertFalse(it.info().isScroll)}
        }finally{reader.shutdown();reader.close();fixture.delete()}
    }
    private suspend fun checkNativeHandles(reader:MobileReader,info:PageInfo){
        val visible=info.regions.filter {it.bounds.any {b->b.y>40f&&b.y+b.height<580f}}
        assertTrue(visible.size>=2)
        val a=visible[0].bounds.first();val b=visible[1].bounds.first()
        val selected=reader.select(a.x+2,a.y+a.height/2,a.x+25,a.y+a.height/2)!!
        reader.beginSelectionHandle(false)
        val extended=reader.extendSelection(b.x+40,b.y+b.height/2)!!
        assertTrue(extended.text.length>selected.text.length)
        reader.scrollBy(50f).close();assertEquals(extended.text,reader.selection()!!.text)
        reader.clearSelection()
    }
    private fun checkViewGestures(info:PageInfo){
        val instrumentation=InstrumentationRegistry.getInstrumentation()
        instrumentation.runOnMainSync {
            val context=instrumentation.targetContext
            val bitmap=Bitmap.createBitmap(720,1280,Bitmap.Config.ARGB_8888)
            val view=ReaderView(context);view.layout(0,0,720,1280)
            view.present(ReaderFrame(bitmap,info.copy(scrollDistance=0.0)),SelectedText("selected",listOf(Rect(30f,50f,80f,20f))))
            var handle:Boolean?=null;var moved=false;var ended=false;var scrolled=0f
            view.onHandleStart={handle=it};view.onHandleMove={_,_->moved=true};view.onHandleEnd={ended=true};view.onScroll={scrolled+=it}
            val density=context.resources.displayMetrics.density;val start=android.os.SystemClock.uptimeMillis()
            fun event(action:Int,x:Float,y:Float,time:Long){val e=android.view.MotionEvent.obtain(start,time,action,x*density,y*density,0);view.onTouchEvent(e);e.recycle()}
            event(0,110f,76f,start);event(2,135f,90f,start+30);event(1,135f,90f,start+60)
            assertEquals(false,handle);assertTrue(moved&&ended)
            view.present(ReaderFrame(bitmap,info.copy(scrollDistance=0.0)),null)
            event(0,160f,180f,start+100);event(2,160f,100f,start+140);event(1,160f,100f,start+180)
            assertTrue(scrolled>0f)
            view.onWindowFocusChanged(false);bitmap.recycle()
        }
    }
    @Test(timeout=20000) fun assistantFormatsWhileTokensContinue() {
        val instrumentation=InstrumentationRegistry.getInstrumentation()
        val text=androidx.compose.runtime.mutableStateOf("**持续回复**")
        var formatted=false
        fun inspect(view:android.view.View) {
            if(view is android.widget.TextView&&view.text.toString().contains("持续回复")){
                val spans=(view.text as? android.text.Spanned)?.getSpans(0,view.text.length,android.text.style.StyleSpan::class.java).orEmpty()
                if(spans.any {it.style==android.graphics.Typeface.BOLD})formatted=true
            }
            if(view is android.view.ViewGroup)for(i in 0 until view.childCount)inspect(view.getChildAt(i))
        }
        // Launch our isolated preview directly. Some OEMs block the test-core
        // bootstrap activity's background launch/broadcast handoff.
        val monitor=instrumentation.addMonitor(MainActivity::class.java.name,null,false)
        var activity:MainActivity?=null
        try {
            android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("am start -W -f 0x10008000 -n org.tortotech.torto.debug/org.tortotech.torto.MainActivity")).use {it.readBytes()}
            activity=monitor.waitForActivityWithTimeout(5000) as? MainActivity
            assertNotNull("Preview activity must start within five seconds",activity)
            val host=activity!!
            instrumentation.runOnMainSync {host.setContent {
                androidx.compose.material3.MaterialTheme {
                    val cache=androidx.compose.runtime.remember {AssistantAssetCache()}
                    AssistantRichText(text.value,cache,true){}
                }
            }}
            // Keep producing tokens faster than the formatter's cadence. A
            // trailing debounce would never produce styled text during this run.
            repeat(28){index->
                instrumentation.runOnMainSync {text.value="**持续回复** 内容 "+"续".repeat(index);inspect(host.window.decorView)}
                android.os.SystemClock.sleep(25)
            }
            assertTrue("Markdown must be formatted before a continuous stream ends",formatted)
        }finally {
            activity?.let {host->instrumentation.runOnMainSync {host.finish()}}
            instrumentation.removeMonitor(monitor)
        }
    }
    @Test fun assistantImagesHistoryAndCitationsRoundtrip()=runBlocking {
        val context=InstrumentationRegistry.getInstrumentation().targetContext
        val fixture=File(context.cacheDir,"assistant-fixture.fb2")
        val paragraphs=(0..25).joinToString(""){ "<p>段落 $it：这是用于核对阅读助手引用位置的测试内容。</p>" }
        fixture.writeText("""<?xml version="1.0" encoding="UTF-8"?><FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0"><description><title-info><book-title>阅读助手测试</book-title><lang>zh</lang></title-info></description><body><section><title><p>阅读助手</p></title>$paragraphs</section></body></FictionBook>""")
        val imageFile=File(context.cacheDir,"assistant-attachment.png")
        val bitmap=Bitmap.createBitmap(160,100,Bitmap.Config.ARGB_8888).apply {eraseColor(android.graphics.Color.rgb(30,140,100))}
        imageFile.outputStream().use {bitmap.compress(Bitmap.CompressFormat.PNG,100,it)};bitmap.recycle()
        val root=File(context.filesDir,"core")
        MobileCore(root.absolutePath).use {core->
            val book=core.importBook(fixture.absolutePath).first {it.title=="阅读助手测试"}
            val reader=core.openBook(book.id,Viewport(360u,640u,2f))
            val attachment:ChatAttachment
            val citation:String
            try {
                attachment=reader.addChatImage(imageFile.absolutePath,"设备附件测试.png")
                reader.chatImage(attachment.id).use {assertEquals(160u,it.width());assertEquals(100u,it.height())}
                val source=org.json.JSONObject(reader.search("段落 12：").single().source).getJSONObject("start")
                citation="link://j/0/"+android.net.Uri.encode(source.getString("node"))
                reader.followChatCitation(citation).use {assertTrue(it.info().regions.any {r->r.text.contains("段落 12：")});assertTrue(it.info().canGoBack)}
                reader.goBack().use {assertFalse(it.info().canGoBack)}
                try{reader.chatImage("../../secret").close();fail("Invalid image identifier must fail")}catch(_:CoreException){}
            }finally{reader.shutdown();reader.close()}
            val markdown="## 阅读助手\n\n**重点内容**、*强调*和 `代码`，行内公式 \$x^2\$\n\n> 这是一段引用\n\n- 第一项\n- 第二项\n\n|项目|数值|\n|---|---|\n|甲|10|\n|乙|20|\n\n\$\$\\frac{1}{2}\$\$\n\n[查看第 12 段]("+citation+")"
            val parsed=decodeAssistantMarkdown(chatMarkdown(markdown))
            assertTrue(parsed.any {it.kind=="table"&&it.rows.size==3})
            assertTrue(parsed.flatMap {it.runs}.any {it.math})
            assertTrue(parsed.flatMap {it.runs}.any {it.link==citation})
            val messages=org.json.JSONArray().put(org.json.JSONObject().put("user",true).put("content","请结合图片和书中内容解释").put("images",org.json.JSONArray().put(org.json.JSONObject().put("id",attachment.id).put("label",attachment.label)))).put(org.json.JSONObject().put("user",false).put("content",markdown))
            val history=File(root,"chat/${book.id}.json");history.parentFile!!.mkdirs();history.writeText(messages.toString())
            core.openBook(book.id,Viewport(360u,640u,2f)).use {reopened->
                try {
                    assertEquals(2,reopened.chatHistory().size)
                    assertEquals(attachment.id,reopened.chatHistory()[0].images.single().id)
                    reopened.removeChatImage(attachment.id)
                    reopened.chatImage(attachment.id).close()
                }finally{reopened.shutdown()}
            }
        }
        fixture.delete();imageFile.delete()
        Unit
    }
    @Test fun focusSurroundingsFadeToPaperInBothThemes()=runBlocking {
        val instrumentation=InstrumentationRegistry.getInstrumentation()
        val context=instrumentation.targetContext
        val fixture=File(context.cacheDir,"focus-colors.epub")
        instrumentation.context.assets.open("reader-interactions.epub").use {input->fixture.outputStream().use {input.copyTo(it)}}
        MobileCore(File(context.filesDir,"core").absolutePath).use {core->
            val book=core.importBook(fixture.absolutePath).first {it.title=="阅读交互测试"}
            val reader=core.openBook(book.id,Viewport(360u,640u,2f))
            try {
                for(dark in listOf(false,true))reader.configure(20f,false,dark).use {page->
                    val info=page.info().copy(focusBounds=listOf(Rect(10f,10f,20f,20f)))
                    assertEquals(140,info.focusFadeColor.toInt() ushr 24)
                    instrumentation.runOnMainSync {
                        val density=context.resources.displayMetrics.density
                        val size=(100*density).toInt()
                        val paper=info.focusFadeColor.toInt() or (0xFF shl 24)
                        val ink=if(dark)android.graphics.Color.WHITE else android.graphics.Color.BLACK
                        val bitmap=Bitmap.createBitmap(size,size,Bitmap.Config.ARGB_8888)
                        val output=Bitmap.createBitmap(size,size,Bitmap.Config.ARGB_8888)
                        try {
                            bitmap.eraseColor(paper)
                            val paint=android.graphics.Paint().apply {color=ink}
                            val input=android.graphics.Canvas(bitmap)
                            for(position in listOf(15f,70f))input.drawRect(position*density,position*density,(position+10)*density,(position+10)*density,paint)
                            val view=ReaderView(context)
                            view.layout(0,0,size,size)
                            view.present(ReaderFrame(bitmap,info),null)
                            view.draw(android.graphics.Canvas(output))
                            // Android's 8-bit alpha compositing can round a
                            // same-color blend down by one channel value.
                            val background=output.getPixel(0,0)
                            for(shift in listOf(0,8,16))assertTrue("Paper must retain its color",kotlin.math.abs(((paper ushr shift) and 255)-((background ushr shift) and 255))<=1)
                            assertEquals("Active content must retain full opacity",ink,output.getPixel((20*density).toInt(),(20*density).toInt()))
                            val faded=output.getPixel((75*density).toInt(),(75*density).toInt())
                            for(shift in listOf(0,8,16)) {
                                val expected=(((ink ushr shift) and 255)*115+((paper ushr shift) and 255)*140+127)/255
                                assertTrue("Inactive ink must fade toward its own paper",kotlin.math.abs(((faded ushr shift) and 255)-expected)<=2)
                            }
                            view.present(ReaderFrame(bitmap,info.copy(focusBounds=emptyList())),null)
                            view.draw(android.graphics.Canvas(output))
                            assertEquals("Disabling focus must restore content",ink,output.getPixel((75*density).toInt(),(75*density).toInt()))
                        }finally{bitmap.recycle();output.recycle()}
                    }
                }
            }finally{reader.shutdown();reader.close()}
        }
        fixture.delete()
        Unit
    }
    @Test fun secretsAreEncryptedAndBoundToTheirAccount() {
        val context=InstrumentationRegistry.getInstrumentation().targetContext
        val storage=KeystoreStorage(context)
        val secret="fixture-secret-密钥"
        try {
            storage.write("test","first",secret)
            assertEquals(secret,storage.read("test","first"))
            assertNull(storage.read("test","second"))
            val ciphertext=context.getSharedPreferences("credentials",0).all.values.joinToString()
            assertFalse(ciphertext.contains(secret))
            storage.delete("test","first")
            assertNull(storage.read("test","first"))
        } finally { storage.delete("test","first") }
    }
    @Test fun previewPinchZoomAndDoubleTapReset(){
        val instrumentation=InstrumentationRegistry.getInstrumentation()
        instrumentation.runOnMainSync {
            val image=Bitmap.createBitmap(64,64,Bitmap.Config.ARGB_8888).apply {eraseColor(android.graphics.Color.GREEN)}
            val view=ZoomImageView(instrumentation.targetContext,image)
            val minimum=if(android.os.Build.VERSION.SDK_INT>=29)android.view.ViewConfiguration.get(instrumentation.targetContext).scaledMinimumScalingSpan.toFloat() else 240f
            val initialSpan=(minimum+40f).coerceAtLeast(240f)
            val width=(initialSpan*1.5f+160f).toInt().coerceAtLeast(1080)
            val height=width*16/9
            val centerX=width/2f;val centerY=height/2f
            view.layout(0,0,width,height)
            fun snapshot()=Bitmap.createBitmap(width,height,Bitmap.Config.ARGB_8888).also {view.draw(android.graphics.Canvas(it))}
            val before=snapshot()
            var clock=android.os.SystemClock.uptimeMillis()
            var gestureStart=clock
            fun event(action:Int,points:List<Pair<Float,Float>>){
                if(action==android.view.MotionEvent.ACTION_DOWN)gestureStart=clock
                val properties=Array(points.size){index->android.view.MotionEvent.PointerProperties().apply {id=index;toolType=android.view.MotionEvent.TOOL_TYPE_FINGER}}
                val coords=Array(points.size){index->android.view.MotionEvent.PointerCoords().apply {x=points[index].first;y=points[index].second;pressure=1f;size=1f}}
                val event=android.view.MotionEvent.obtain(gestureStart,clock,action,points.size,properties,coords,0,0,1f,1f,0,0,android.view.InputDevice.SOURCE_TOUCHSCREEN,0)
                view.onTouchEvent(event);event.recycle();clock+=20
            }
            fun pointers(span:Float)=listOf((centerX-span/2) to centerY,(centerX+span/2) to centerY)
            event(android.view.MotionEvent.ACTION_DOWN,listOf(pointers(initialSpan)[0]))
            event(android.view.MotionEvent.ACTION_POINTER_DOWN or (1 shl 8),pointers(initialSpan))
            for(factor in listOf(1.1f,1.2f,1.3f,1.4f,1.5f))event(android.view.MotionEvent.ACTION_MOVE,pointers(initialSpan*factor))
            event(android.view.MotionEvent.ACTION_POINTER_UP or (1 shl 8),pointers(initialSpan*1.5f))
            event(android.view.MotionEvent.ACTION_UP,listOf(pointers(initialSpan*1.5f)[0]))
            val zoomed=snapshot()
            assertFalse("Pinch must change the rendered viewport",before.sameAs(zoomed))
            clock+=700
            repeat(2){event(android.view.MotionEvent.ACTION_DOWN,listOf(centerX to centerY));event(android.view.MotionEvent.ACTION_UP,listOf(centerX to centerY));clock+=50}
            val reset=snapshot();assertTrue("Double tap must restore fitted image",before.sameAs(reset))
            before.recycle();zoomed.recycle();reset.recycle();image.recycle()
        }
    }
    @Test fun focusLinksAndTouchTargetsUseNativeSourcePositions()=runBlocking {
        val instrumentation=InstrumentationRegistry.getInstrumentation()
        val context=instrumentation.targetContext
        val fixture=File(context.cacheDir,"reader-interactions.epub")
        instrumentation.context.assets.open("reader-interactions.epub").use {input->fixture.outputStream().use {input.copyTo(it)}}
        MobileCore(File(context.filesDir,"core").absolutePath).use {core->
            val book=core.importBook(fixture.absolutePath).first {it.title=="阅读交互测试"}
            val reader=core.openBook(book.id,Viewport(360u,640u,2f))
            try{
                reader.page().use {page->
                    assertTrue(page.info().regions.any {r->r.text.contains("Jump internally")})
                    val label=page.info().regions.first {it.text.contains("Inline formula:")}.bounds.last()
                    // Text ranges may include the inline object's source slot;
                    // probe its trailing neighbourhood instead of assuming the
                    // fraction lies strictly outside the text rectangle.
                    var formula:TouchAction?=null
                    search@ for(dy in -32..32 step 4)for(dx in -32..32 step 4){
                        val candidate=reader.activate(label.x+label.width+dx,label.y+label.height/2+dy)
                        if(candidate.kind=="formula"){formula=candidate;break@search}
                        candidate.media?.close()
                    }
                    assertNotNull("Missing formula touch target near $label",formula)
                    formula!!.media!!.use {preview->
                            val bitmap=Bitmap.createBitmap(preview.width().toInt(),preview.height().toInt(),Bitmap.Config.ARGB_8888)
                            try {
                                assertTrue(NativePixels.copyPage(preview.id().toLong(),bitmap))
                                val pixels=IntArray(bitmap.width*bitmap.height)
                                bitmap.getPixels(pixels,0,bitmap.width,0,0,bitmap.width,bitmap.height)
                                val paintedRows=(0 until bitmap.height).count {y->(0 until bitmap.width).any {x->pixels[y*bitmap.width+x] ushr 24 > 0}}
                                assertTrue("Fraction must include numerator and denominator, not only its rule",paintedRows>bitmap.height/4)
                            } finally {bitmap.recycle()}
                    }
                }
                reader.focusMode(true).use {assertTrue(it.info().focusCount>=2u);assertTrue(it.info().focusBounds.isNotEmpty())}
                reader.turn(true).use {assertEquals(1u,it.info().focusIndex)}
                reader.turn(false).use {assertEquals(0u,it.info().focusIndex)}
                reader.focusMode(false).use {assertTrue(it.info().focusBounds.isEmpty())}
                reader.followLink("body.xhtml#destination").use {assertTrue(it.info().canGoBack)}
                reader.goBack().use {assertFalse(it.info().canGoBack)}
                val control=ChatRequest()
                try{
                    control.cancel()
                    try{reader.translatePageCancellable(true,control).close();fail("Expected cancelled processing")}
                    catch(e:CoreException){assertTrue(e.message.orEmpty().contains("已停止"))}
                }finally{control.close()}
            }finally{reader.shutdown();reader.close()}
        }
        fixture.delete()
        Unit
    }
    @Test fun sharedCoreRendersSearchesSelectsAndRestores() = runBlocking {
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        val fixture = File(context.cacheDir,"android-integration.fb2")
        val paragraphs = (0..60).joinToString("") { "<p>Paragraph $it: A shared reading engine keeps source locations stable. 中文排版与搜索。</p>" }
        fixture.writeText("""<?xml version="1.0" encoding="UTF-8"?><FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0"><description><title-info><book-title>阅读引擎测试</book-title><lang>en</lang></title-info></description><body><section id="one"><title><p>Chapter One</p></title>$paragraphs</section></body></FictionBook>""")
        MobileCore(File(context.filesDir,"core").absolutePath).use { core ->
            val book = core.importBook(fixture.absolutePath).first { it.title == "阅读引擎测试" }
            val viewport=Viewport(360u,640u,2f)
            val reader=core.openBook(book.id,viewport)
            try {
                reader.page().use { page ->
                    assertEquals(720u,page.width()); assertEquals(1280u,page.height())
                    val bitmap=Bitmap.createBitmap(720,1280,Bitmap.Config.ARGB_8888)
                    assertTrue(NativePixels.copyPage(page.id().toLong(),bitmap))
                    assertFalse(NativePixels.copyPage(Long.MAX_VALUE,bitmap))
                    val mismatched=Bitmap.createBitmap(1,1,Bitmap.Config.ARGB_8888)
                    assertFalse(NativePixels.copyPage(page.id().toLong(),mismatched))
                    assertTrue(page.info().regions.any { it.text.contains("Paragraph") })
                }
                val hit=reader.search("Paragraph 45").single()
                reader.goToSource(hit.source).use { page ->
                    val region=page.info().regions.first { it.text.contains("Paragraph 45") }
                    val rect=region.bounds.first()
                    val selected=reader.select(rect.x+2f,rect.y+rect.height/2,rect.x+45f,rect.y+rect.height/2)
                    assertNotNull(selected); assertTrue(selected!!.text.isNotBlank())
                    reader.selectionMode("paragraph")
                    val notes=reader.saveAnnotation("设备端批注测试")
                    val note=notes.last { it.note=="设备端批注测试" }
                    reader.page().use { assertTrue(it.info().marks.isNotEmpty()) }
                    reader.editAnnotation(note.id,"edited on device")
                    assertEquals("edited on device",reader.annotations().first { it.id==note.id }.note)
                    reader.clearSelection()
                    reader.deleteAnnotation(note.id)
                    val request=ChatRequest()
                    try {
                        request.cancel()
                        try { reader.chat("test cancellation",request,object:ChatListener { override fun update(content:String,reasoning:String,activity:String) { fail("cancelled callback") } });fail("Expected cancellation") }
                        catch(e:CoreException) { assertTrue(e.message.orEmpty().contains("已停止")) }
                    } finally { request.close() }
                    assertEquals(7,core.statistics().distribution.size)
                }
                reader.resize(Viewport(600u,360u,2f)).use { page -> assertEquals(1200u,page.width()) }
            } finally {reader.shutdown();reader.close()}
            core.openBook(book.id,viewport).use { reopened ->
                reopened.page().use { page -> assertTrue(page.info().progress>0) }
                reopened.shutdown()
            }
        }
        fixture.delete()
        Unit
    }
}
