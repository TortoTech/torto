package org.tortotech.torto

import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import kotlin.math.roundToInt
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Test
import org.junit.runner.RunWith
import org.tortotech.torto.core.PageInfo
import org.tortotech.torto.core.Rect
import org.tortotech.torto.core.SelectedText

@RunWith(AndroidJUnit4::class)
class ReaderSelectionRenderingTest {
    @Test
    fun adjacentSelectionLinesApplyTheSameColorInTheOverlap() {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        instrumentation.runOnMainSync {
            val view = ReaderView(instrumentation.targetContext)
            val density = view.resources.displayMetrics.density
            fun px(dp: Int) = (dp * density).roundToInt()
            view.layout(0, 0, px(120), px(110))
            val paper = Bitmap.createBitmap(view.width, view.height, Bitmap.Config.ARGB_8888)
            val rendered = Bitmap.createBitmap(view.width, view.height, Bitmap.Config.ARGB_8888)
            try {
                paper.eraseColor(Color.WHITE)
                val frame = ReaderFrame(paper, PageInfo(
                    section = 0u, sectionCount = 1u, page = 0u, pageCount = 1u, progress = 0.0,
                    regions = emptyList(), marks = emptyList(), focusBounds = emptyList(),
                    focusFadeColor = 0u, focusHighlight = false, isScroll = false,
                    scrollDistance = 0.0, scrollContext = 0uL, rasterTop = 0f, rasterHeight = 110f,
                    previousFocus = null, nextFocus = null, controls = emptyList(),
                    activeTocId = null, activeTocPath = emptyList(), focusIndex = 0u,
                    focusCount = 0u, canGoBack = false, focusChatKey = "", focusChatBadges = emptyList(),
                ))
                val selection = SelectedText(
                    "A wrapped paragraph",
                    listOf(Rect(30f, 20f, 65f, 30f), Rect(10f, 48f, 55f, 30f)),
                    highlighted = false,
                )
                fun pixel(x: Int, y: Int) = rendered.getPixel(px(x), px(y))
                view.present(frame, selection)
                view.draw(Canvas(rendered))
                val selected = pixel(45, 35)
                assertNotEquals(Color.WHITE, selected)
                assertEquals(selected, pixel(45, 49))
                assertEquals(selected, pixel(45, 62))
                assertEquals(Color.WHITE, pixel(16, 35))
                assertEquals(Color.WHITE, pixel(85, 62))

                view.present(frame, null)
                view.draw(Canvas(rendered))
                assertEquals(Color.WHITE, pixel(45, 49))
            } finally {
                rendered.recycle()
                paper.recycle()
            }
        }
    }
}
