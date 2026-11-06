package org.tortotech.torto

import android.graphics.Bitmap
import android.graphics.Canvas
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import kotlinx.coroutines.runBlocking
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.tortotech.torto.core.*
import java.io.File
import java.util.zip.ZipEntry
import java.util.zip.ZipOutputStream
import kotlin.math.roundToInt

@RunWith(AndroidJUnit4::class)
class FocusContentTest {
    @Test fun headingsTableNotesAndFigureCreditsShareTheNativeFocusMask():Unit=runBlocking {
        val instrumentation=InstrumentationRegistry.getInstrumentation()
        val context=instrumentation.targetContext
        val core=MobileCore(File(context.filesDir,"core").absolutePath)
        val reading=core.readingOptions();val ai=core.aiOptions();val before=core.books().map {it.id}.toSet()
        val fixture=File(context.cacheDir,"focus-content.epub")
        val body="<h1>Chapter</h1><p>Earlier discussion.</p><h2>Topic title</h2><h3>Subtopic title</h3><p>Topic body.</p><p>Unrelated discussion.</p><h2>Table topic</h2><p>Table 1. Values</p><table><tr><td>Value</td><td>42</td></tr></table><footer><p>Source: Table credit.</p><p>Note: Rounded values.</p></footer><p>After table.</p><h2>Figure topic</h2><figure><img src='diagram.png' alt='Diagram' width='120' height='60'/><figcaption>Figure 1. Diagram</figcaption><p>Source: Figure credit.</p></figure><p>After figure.</p><h2>Front figure topic</h2><div><p>Figure 2. Front diagram</p><img src='diagram.png' alt='Diagram' width='120' height='60'/><p>Source: Front credit.</p></div><p>After front figure.</p>"
        val files=mapOf(
            "mimetype" to "application/epub+zip",
            "META-INF/container.xml" to """<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="book.opf"/></rootfiles></container>""",
            "book.opf" to """<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="id">android-focus-content</dc:identifier><dc:title>Focus content fixture</dc:title><dc:language>en</dc:language></metadata><manifest><item id="body" href="body.xhtml" media-type="application/xhtml+xml"/><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/><item id="diagram" href="diagram.png" media-type="image/png"/></manifest><spine><itemref idref="body"/></spine></package>""",
            "body.xhtml" to "<html xmlns='http://www.w3.org/1999/xhtml'><body>$body</body></html>",
            "nav.xhtml" to """<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="body.xhtml">Chapter</a></li></ol></nav></body></html>""",
            "diagram.png" to ""
        )
        val png=java.io.ByteArrayOutputStream()
        Bitmap.createBitmap(120,60,Bitmap.Config.ARGB_8888).let {bitmap->bitmap.eraseColor(0xff398975.toInt());bitmap.compress(Bitmap.CompressFormat.PNG,100,png);bitmap.recycle()}
        ZipOutputStream(fixture.outputStream()).use {zip->files.forEach {(name,value)->zip.putNextEntry(ZipEntry(name));zip.write(if(name=="diagram.png")png.toByteArray()else value.toByteArray());zip.closeEntry()}}
        var imported:String?=null
        var reader:MobileReader?=null
        try {
            core.saveAiOptions(ai.copy(layoutEnabled=false));core.saveReadingOptions(reading.copy(translated=false,sentenceSplit=false))
            val book=core.importBook(fixture.absolutePath).first {it.title=="Focus content fixture"};imported=book.id
            val session=core.openBook(book.id,Viewport(360u,640u,2f));reader=session
            session.focusMode(true).close()
            for(dark in listOf(false,true)) {
                session.configure(20f,true,dark).close()
                for((query,related,unrelated) in listOf(
                    Triple("Topic body",listOf("Topic title","Subtopic title","Topic body."),"Unrelated discussion."),
                    Triple("Table credit",listOf("Table topic","Table 1. Values","Source: Table credit.","Note: Rounded values."),"After table."),
                    Triple("Figure credit",listOf("Figure topic","Figure 1. Diagram","Source: Figure credit."),"After figure."),
                    Triple("Front credit",listOf("Front figure topic","Figure 2. Front diagram","Source: Front credit."),"After front figure.")
                )) {
                    val hit=session.search(query).single()
                    session.goToSource(hit.source).use {page->
                        val info=page.info()
                        assertEquals("The active body and preceding titles use separate clear regions",2,info.focusBounds.size)
                        fun contains(rect:Rect)=info.focusBounds.any {r->rect.x>=r.x-.1f&&rect.y>=r.y-.1f&&rect.x+rect.width<=r.x+r.width+.1f&&rect.y+rect.height<=r.y+r.height+.1f}
                        for(text in related) {
                            val regions=info.regions.filter {it.text.contains(text)}
                            assertTrue("Related content must be visible in this fixture: $text",regions.isNotEmpty())
                            assertTrue("Related headings/captions/notes/credits stay clear: $text",regions.flatMap {it.bounds}.all(::contains))
                        }
                        assertTrue("Following prose remains independent",info.regions.filter {it.text.contains(unrelated)}.flatMap {it.bounds}.none(::contains))
                        val source=Bitmap.createBitmap(page.width().toInt(),page.height().toInt(),Bitmap.Config.ARGB_8888)
                        val output=Bitmap.createBitmap(720,1280,Bitmap.Config.ARGB_8888)
                        try {
                            assertTrue(NativePixels.copyPage(page.id().toLong(),source))
                            instrumentation.runOnMainSync {
                                val view=ReaderView(context);view.layout(0,0,720,1280)
                                // A 2x native viewport matches this view's raster scale.
                                val density=context.resources.displayMetrics.density
                                val scaled=info.copy(scrollDistance=info.scrollDistance*2/density,rasterTop=info.rasterTop*2/density,rasterHeight=info.rasterHeight*2/density,
                                    focusBounds=info.focusBounds.map {r->Rect(r.x*2/density,r.y*2/density,r.width*2/density,r.height*2/density)})
                                view.present(ReaderFrame(source,scaled,page.id()),null);view.draw(Canvas(output));view.onWindowFocusChanged(false)
                            }
                            var checked=0
                            val top=(info.rasterTop*2).roundToInt()
                            for(text in related)for(rect in info.regions.filter {it.text.contains(text)}.flatMap {it.bounds}) {
                                for(y in (rect.y*2).toInt().coerceAtLeast(0) until ((rect.y+rect.height)*2).toInt().coerceAtMost(1280))
                                    for(x in (rect.x*2).toInt().coerceAtLeast(0) until ((rect.x+rect.width)*2).toInt().coerceAtMost(720)) {
                                        assertEquals("Associated ink must not be faded in the actual View: $text",source.getPixel(x,y-top),output.getPixel(x,y));checked++
                                    }
                            }
                            assertTrue(checked>0)
                            if(query in listOf("Figure credit","Front credit")) {
                                var imagePixels=0
                                for(y in 0 until 1280)for(x in 0 until 720)if(source.getPixel(x,y-top)==0xff398975.toInt()&&info.focusBounds.any {r->x>=r.x*2&&x<(r.x+r.width)*2&&y>=r.y*2&&y<(r.y+r.height)*2}) {
                                    assertEquals("The image itself stays clear along with its caption and credit",source.getPixel(x,y-top),output.getPixel(x,y));imagePixels++
                                }
                                assertTrue("The fixture must test the image body, not only its text",imagePixels>1000)
                            }
                            File(context.cacheDir,"focus-content-${query.replace(' ','-')}-${if(dark)"dark"else"light"}.png").outputStream().use {output.compress(Bitmap.CompressFormat.PNG,100,it)}
                        }finally {source.recycle();output.recycle()}
                    }
                }
            }
        }finally {
            reader?.shutdown();reader?.close();core.saveReadingOptions(reading);core.saveAiOptions(ai)
            imported?.takeIf {it !in before}?.let {core.removeBook(it)};core.close();fixture.delete()
        }
    }
}
