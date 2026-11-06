package org.tortotech.torto

import android.content.Context
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Paint
import android.view.GestureDetector
import android.view.MotionEvent
import android.view.ScaleGestureDetector
import android.view.View
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.semantics.dismiss
import androidx.compose.ui.semantics.paneTitle
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.compose.ui.window.DialogWindowProvider
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat

@Composable fun MediaPreview(interaction:ReaderInteraction,close:()->Unit){
    if(interaction.kind=="image"){
        FullScreenImagePreview(interaction.frame?.bitmap,close)
        return
    }
    Dialog(onDismissRequest=close,properties=DialogProperties(usePlatformDefaultWidth=false)){
        Surface(Modifier.fillMaxSize(),color=MaterialTheme.colorScheme.background){
            Column(Modifier.systemBarsPadding()){
                Row(Modifier.fillMaxWidth().padding(horizontal=8.dp),horizontalArrangement=Arrangement.End){ActionIcon("close",tr("关闭预览","Close preview"),onClick=close)}
                interaction.frame?.let {frame->AndroidView(factory={ZoomImageView(it,frame.bitmap,interaction.kind!="formula")},modifier=Modifier.fillMaxWidth().weight(1f))}
                if(interaction.kind=="formula"){
                    var source by remember{mutableStateOf(false)}
                    TextButton(onClick={source=!source}){Text(tr("公式源代码","Formula source"))}
                    if(source)SelectionContainer{Text(interaction.text,Modifier.fillMaxWidth().heightIn(max=160.dp).verticalScroll(rememberScrollState()).padding(16.dp))}
                }
            }
        }
    }
}

/** The image owns a separate edge-to-edge window; closing it restores the reader untouched. */
@Composable internal fun FullScreenImagePreview(bitmap:Bitmap?,close:()->Unit){
    val currentClose by rememberUpdatedState(close)
    val title=tr("图片预览","Image preview")
    val gestures=tr("图片预览，双指缩放，双击缩放或复位，点击空白处或返回关闭","Image preview. Pinch or double tap to zoom. Tap outside the image or go back to close.")
    Dialog(onDismissRequest=close,properties=DialogProperties(usePlatformDefaultWidth=false,decorFitsSystemWindows=false)){
        val view=LocalView.current
        val window=(view.parent as? DialogWindowProvider)?.window
        SideEffect {
            window?.let {
                // Never inherit an OEM's fullscreen-dialog slide/scale transition.
                if(it.attributes.windowAnimations!=R.style.Animation_Torto_ImagePreview)it.setWindowAnimations(R.style.Animation_Torto_ImagePreview)
                if(it.attributes.dimAmount!=0f)it.setDimAmount(0f)
                val bars=WindowCompat.getInsetsController(it,view)
                bars.isAppearanceLightStatusBars=false
                bars.isAppearanceLightNavigationBars=false
                bars.systemBarsBehavior=WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
                bars.hide(WindowInsetsCompat.Type.systemBars())
            }
        }
        Box(Modifier.fillMaxSize().background(Color.Black).semantics {paneTitle=title;dismiss {currentClose();true}}){
            if(bitmap!=null)key(bitmap){
                AndroidView(factory={ZoomImageView(it,bitmap,onDismiss={currentClose()}).apply {contentDescription=gestures}},modifier=Modifier.fillMaxSize())
            }else Box(Modifier.fillMaxSize().clickable {currentClose()})
        }
    }
}
/** Bounded native preview bitmap; only gestures invalidate the view. */
internal class ZoomImageView(context:Context,private val bitmap:Bitmap,private val allowUpscale:Boolean=true,private val onDismiss:(()->Unit)?=null):View(context){
    private val paint=Paint(Paint.ANTI_ALIAS_FLAG or Paint.FILTER_BITMAP_FLAG)
    private var zoom=1f;private var panX=0f;private var panY=0f
    private var lastX=0f;private var lastY=0f
    private val base get()=minOf(width.toFloat()/bitmap.width,height.toFloat()/bitmap.height,if(allowUpscale)Float.MAX_VALUE else 1f)
    private fun clamp(){val maxX=((bitmap.width*base*zoom-width)/2).coerceAtLeast(0f);val maxY=((bitmap.height*base*zoom-height)/2).coerceAtLeast(0f);panX=panX.coerceIn(-maxX,maxX);panY=panY.coerceIn(-maxY,maxY)}
    private val scale=ScaleGestureDetector(context,object:ScaleGestureDetector.SimpleOnScaleGestureListener(){override fun onScale(detector:ScaleGestureDetector):Boolean{
        val next=(zoom*detector.scaleFactor).coerceIn(1f,8f);val ratio=next/zoom
        panX=(panX-(detector.focusX-width/2))*ratio+(detector.focusX-width/2)
        panY=(panY-(detector.focusY-height/2))*ratio+(detector.focusY-height/2)
        zoom=next;clamp();invalidate();return true
    }})
    private val gestures=GestureDetector(context,object:GestureDetector.SimpleOnGestureListener(){
        override fun onDown(e:MotionEvent)=true
        override fun onSingleTapConfirmed(e:MotionEvent):Boolean{
            val halfWidth=bitmap.width*base*zoom/2f;val halfHeight=bitmap.height*base*zoom/2f
            if(e.x<width/2f+panX-halfWidth||e.x>width/2f+panX+halfWidth||e.y<height/2f+panY-halfHeight||e.y>height/2f+panY+halfHeight)onDismiss?.invoke()
            return true
        }
        override fun onDoubleTap(e:MotionEvent):Boolean{zoom=if(zoom>1f)1f else 2.5f;panX=0f;panY=0f;invalidate();return true}
    })
    init {contentDescription="Image preview. Pinch to zoom; double tap to reset.";isFocusable=true}
    override fun onDraw(canvas:Canvas){super.onDraw(canvas);val size=base*zoom;canvas.save();canvas.translate(width/2f+panX,height/2f+panY);canvas.scale(size,size);canvas.drawBitmap(bitmap,-bitmap.width/2f,-bitmap.height/2f,paint);canvas.restore()}
    override fun onTouchEvent(event:MotionEvent):Boolean{
        scale.onTouchEvent(event);gestures.onTouchEvent(event)
        if(event.actionMasked==MotionEvent.ACTION_MOVE&&!scale.isInProgress&&event.pointerCount==1){panX+=event.x-lastX;panY+=event.y-lastY;clamp();invalidate()}
        lastX=event.x;lastY=event.y
        if(event.actionMasked==MotionEvent.ACTION_UP)performClick()
        return true
    }
    override fun performClick():Boolean{super.performClick();return true}
}
