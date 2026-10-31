package org.tortotech.torto

import android.content.Context
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Paint
import android.view.GestureDetector
import android.view.MotionEvent
import android.view.ScaleGestureDetector
import android.view.View
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties

@Composable fun MediaPreview(interaction:ReaderInteraction,close:()->Unit,ask:(()->Unit)?=null){
    Dialog(onDismissRequest=close,properties=DialogProperties(usePlatformDefaultWidth=false)){
        Surface(Modifier.fillMaxSize(),color=MaterialTheme.colorScheme.background){
            Column(Modifier.systemBarsPadding()){
                Row(Modifier.fillMaxWidth().padding(horizontal=8.dp),horizontalArrangement=Arrangement.End){if(ask!=null)TextButton(onClick=ask){Text(tr("询问这张图","Ask about this image"))};Spacer(Modifier.weight(1f));ActionIcon("close",tr("关闭预览","Close preview"),onClick=close)}
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
/** Bounded native preview bitmap; only gestures invalidate the view. */
internal class ZoomImageView(context:Context,private val bitmap:Bitmap,private val allowUpscale:Boolean=true):View(context){
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
    private val gestures=GestureDetector(context,object:GestureDetector.SimpleOnGestureListener(){override fun onDown(e:MotionEvent)=true;override fun onDoubleTap(e:MotionEvent):Boolean{zoom=if(zoom>1f)1f else 2.5f;panX=0f;panY=0f;invalidate();return true}})
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
