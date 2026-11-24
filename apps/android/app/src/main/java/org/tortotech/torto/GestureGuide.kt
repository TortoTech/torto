package org.tortotech.torto

import androidx.compose.animation.core.*
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.drawscope.clipRect
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import kotlinx.coroutines.launch

internal enum class GestureDemoKind {
    FocusStep,
    ChapterSwipe,
    SentenceSplit,
    FocusChat,
    ParagraphHighlight,
    ParagraphNote,
    LongPressSelect,
}

private data class GestureSpec(
    val kind: GestureDemoKind,
    val titleZh: String,
    val titleEn: String,
    val descZh: String? = null,
    val descEn: String? = null,
)

private val focusBrowseGestures = listOf(
    GestureSpec(
        kind = GestureDemoKind.FocusStep,
        titleZh = "切换段落：单指上下滑动",
        titleEn = "Switch paragraph: 1-finger swipe up / down",
    ),
    GestureSpec(
        kind = GestureDemoKind.ChapterSwipe,
        titleZh = "切换章节：单指左右滑动",
        titleEn = "Switch chapter: 1-finger swipe left / right",
    ),
    GestureSpec(
        kind = GestureDemoKind.SentenceSplit,
        titleZh = "按句拆段：双指上下滑动",
        titleEn = "Split sentences: 2-finger swipe up / down",
        descZh = "下滑拆句，上滑还原",
        descEn = "Swipe down to split, swipe up to restore",
    ),
)

private val paragraphActionGestures = listOf(
    GestureSpec(
        kind = GestureDemoKind.FocusChat,
        titleZh = "AI聊天：双指点击",
        titleEn = "AI chat: 2-finger tap",
    ),
    GestureSpec(
        kind = GestureDemoKind.ParagraphHighlight,
        titleZh = "高亮段落：双指左右滑动",
        titleEn = "Highlight paragraph: 2-finger swipe left / right",
        descZh = "右滑高亮，左滑还原",
        descEn = "Swipe right to highlight, swipe left to restore",
    ),
    GestureSpec(
        kind = GestureDemoKind.ParagraphNote,
        titleZh = "添加笔记：三指左右滑动",
        titleEn = "Add note: 3-finger swipe left / right",
        descZh = "右滑添加，左滑还原",
        descEn = "Swipe right to add, swipe left to restore",
    ),
)

private val generalGestures = listOf(
    GestureSpec(
        kind = GestureDemoKind.LongPressSelect,
        titleZh = "选择文本：长按并拖拽",
        titleEn = "Select text: Long-press & drag",
    ),
)

@Composable
internal fun GestureOnboardingDialog(onDismiss: () -> Unit) {
    val pagerState = rememberPagerState(pageCount = { 2 })
    val scope = rememberCoroutineScope()
    val transition = rememberInfiniteTransition(label = "gestureOnboardingAnim")
    val progress by transition.animateFloat(
        initialValue = 0f,
        targetValue = 1f,
        animationSpec = infiniteRepeatable(
            animation = tween(durationMillis = 2400, easing = LinearEasing),
            repeatMode = RepeatMode.Restart,
        ),
        label = "gesturePhase",
    )

    Dialog(
        onDismissRequest = {},
        properties = DialogProperties(
            dismissOnBackPress = false,
            dismissOnClickOutside = false,
            usePlatformDefaultWidth = false,
        ),
    ) {
        Box(
            modifier = Modifier
                .fillMaxSize()
                .padding(horizontal = 20.dp, vertical = 24.dp),
            contentAlignment = Alignment.Center,
        ) {
            Surface(
                modifier = Modifier
                    .fillMaxWidth()
                    .widthIn(max = 420.dp)
                    .clickable(
                        interactionSource = remember { MutableInteractionSource() },
                        indication = null,
                        onClick = {},
                    ),
                shape = RoundedCornerShape(24.dp),
                color = MaterialTheme.colorScheme.surface,
                tonalElevation = 4.dp,
                shadowElevation = 12.dp,
            ) {
                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(start = 20.dp, end = 20.dp, top = 20.dp, bottom = 18.dp),
                ) {
                    Text(
                        text = tr("专注模式操作指南", "Focus mode guide"),
                        style = MaterialTheme.typography.titleLarge,
                        fontWeight = FontWeight.SemiBold,
                        fontSize = 20.sp,
                        color = MaterialTheme.colorScheme.onSurface,
                    )

                    Spacer(Modifier.height(16.dp))

                    HorizontalPager(
                        state = pagerState,
                        modifier = Modifier.fillMaxWidth(),
                    ) { page ->
                        val items = if (page == 0) focusBrowseGestures else paragraphActionGestures
                        Column(
                            modifier = Modifier.fillMaxWidth(),
                            verticalArrangement = Arrangement.spacedBy(12.dp),
                        ) {
                            items.forEach { spec ->
                                GestureGuideCardRow(spec = spec, progress = progress)
                            }
                        }
                    }

                    Spacer(Modifier.height(18.dp))

                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.Center,
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        repeat(2) { idx ->
                            val active = pagerState.currentPage == idx
                            Box(
                                modifier = Modifier
                                    .padding(horizontal = 3.dp)
                                    .height(6.dp)
                                    .width(if (active) 18.dp else 6.dp)
                                    .clip(CircleShape)
                                    .background(
                                        if (active) MaterialTheme.colorScheme.primary
                                        else MaterialTheme.colorScheme.outlineVariant,
                                    ),
                            )
                        }
                    }

                    Spacer(Modifier.height(14.dp))

                    Button(
                        onClick = {
                            if (pagerState.currentPage == 0) {
                                scope.launch { pagerState.animateScrollToPage(1) }
                            } else {
                                onDismiss()
                            }
                        },
                        modifier = Modifier
                            .fillMaxWidth()
                            .height(46.dp),
                        shape = CircleShape,
                    ) {
                        Text(
                            text = if (pagerState.currentPage == 0) tr("下一步", "Next")
                            else tr("开始阅读", "Start reading"),
                            fontSize = 15.sp,
                            fontWeight = FontWeight.Medium,
                        )
                    }

                    Spacer(Modifier.height(8.dp))

                    Text(
                        text = tr("可在「设置 → 快捷手势」中回顾", "Review in Settings → Gesture shortcuts"),
                        modifier = Modifier.fillMaxWidth(),
                        textAlign = TextAlign.Center,
                        fontSize = 11.sp,
                        color = MaterialTheme.colorScheme.onSurfaceVariant.copy(
                            alpha = if (pagerState.currentPage == 1) 0.75f else 0f,
                        ),
                    )
                }
            }
        }
    }
}

@Composable
internal fun GestureGuideScreen() {
    val transition = rememberInfiniteTransition(label = "gestureScreenAnim")
    val progress by transition.animateFloat(
        initialValue = 0f,
        targetValue = 1f,
        animationSpec = infiniteRepeatable(
            animation = tween(durationMillis = 2400, easing = LinearEasing),
            repeatMode = RepeatMode.Restart,
        ),
        label = "gestureScreenPhase",
    )

    Column(
        modifier = Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(start = 16.dp, end = 16.dp, top = 8.dp, bottom = 24.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        SectionLabel(tr("专注浏览", "Focus navigation"))
        SettingsGroup {
            focusBrowseGestures.forEachIndexed { idx, spec ->
                if (idx > 0) {
                    HorizontalDivider(
                        modifier = Modifier.padding(horizontal = 16.dp),
                        thickness = 0.5.dp,
                        color = MaterialTheme.colorScheme.outlineVariant.copy(alpha = 0.4f),
                    )
                }
                Box(Modifier.padding(horizontal = 14.dp, vertical = 12.dp)) {
                    GestureGuideCardRow(spec = spec, progress = progress)
                }
            }
        }

        SectionLabel(tr("段落操作", "Paragraph actions"))
        SettingsGroup {
            paragraphActionGestures.forEachIndexed { idx, spec ->
                if (idx > 0) {
                    HorizontalDivider(
                        modifier = Modifier.padding(horizontal = 16.dp),
                        thickness = 0.5.dp,
                        color = MaterialTheme.colorScheme.outlineVariant.copy(alpha = 0.4f),
                    )
                }
                Box(Modifier.padding(horizontal = 14.dp, vertical = 12.dp)) {
                    GestureGuideCardRow(spec = spec, progress = progress)
                }
            }
        }

        SectionLabel(tr("通用手势", "General"))
        SettingsGroup {
            generalGestures.forEach { spec ->
                Box(Modifier.padding(horizontal = 14.dp, vertical = 12.dp)) {
                    GestureGuideCardRow(spec = spec, progress = progress)
                }
            }
        }
    }
}

@Composable
private fun GestureGuideCardRow(spec: GestureSpec, progress: Float) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        GestureMiniStage(
            kind = spec.kind,
            progress = progress,
            modifier = Modifier.size(width = 88.dp, height = 64.dp),
        )
        Column(Modifier.weight(1f)) {
            Text(
                text = tr(spec.titleZh, spec.titleEn),
                fontSize = 15.sp,
                lineHeight = 20.sp,
                fontWeight = FontWeight.Medium,
                color = MaterialTheme.colorScheme.onSurface,
            )
            if (spec.descZh != null && spec.descEn != null) {
                Spacer(Modifier.height(3.dp))
                Text(
                    text = tr(spec.descZh, spec.descEn),
                    fontSize = 12.5.sp,
                    lineHeight = 17.sp,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

@Composable
private fun GestureMiniStage(
    kind: GestureDemoKind,
    progress: Float,
    modifier: Modifier = Modifier,
) {
    val stageBg = MaterialTheme.colorScheme.surfaceContainerLow
    val stageBorder = MaterialTheme.colorScheme.outlineVariant.copy(alpha = 0.55f)
    val inkActive = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.78f)
    val inkDim = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.18f)
    val primary = MaterialTheme.colorScheme.primary
    val surface = MaterialTheme.colorScheme.surface
    val highlightFill = Color(0x66D8B86A)

    Box(
        modifier = modifier
            .clip(RoundedCornerShape(12.dp))
            .background(stageBg)
            .border(1.dp, stageBorder, RoundedCornerShape(12.dp)),
    ) {
        Canvas(Modifier.fillMaxSize()) {
            clipRect {
                when (kind) {
                    GestureDemoKind.FocusStep -> drawFocusStepDemo(progress, inkActive, inkDim, primary)
                    GestureDemoKind.ChapterSwipe -> drawChapterSwipeDemo(progress, inkActive, inkDim, primary, stageBorder)
                    GestureDemoKind.SentenceSplit -> drawSentenceSplitDemo(progress, inkActive, primary)
                    GestureDemoKind.FocusChat -> drawFocusChatDemo(progress, inkActive, inkDim, primary, surface, stageBorder)
                    GestureDemoKind.ParagraphHighlight -> drawHighlightDemo(progress, inkActive, inkDim, primary, highlightFill)
                    GestureDemoKind.ParagraphNote -> drawNoteDemo(progress, inkActive, inkDim, primary, surface)
                    GestureDemoKind.LongPressSelect -> drawLongPressDemo(progress, inkActive, inkDim, primary)
                }
            }
        }
    }
}

private fun smoothStep(edge0: Float, edge1: Float, x: Float): Float {
    val t = ((x - edge0) / (edge1 - edge0)).coerceIn(0f, 1f)
    return t * t * (3f - 2f * t)
}

private fun DrawScope.drawParagraphLines(
    left: Float,
    top: Float,
    width: Float,
    lineHeight: Float,
    gap: Float,
    color: Color,
    ratios: FloatArray = floatArrayOf(1f, 0.92f, 0.65f),
) {
    ratios.forEachIndexed { idx, r ->
        val y = top + idx * (lineHeight + gap)
        drawRoundRect(
            color = color,
            topLeft = Offset(left, y),
            size = Size(width * r, lineHeight),
            cornerRadius = CornerRadius(lineHeight / 2f, lineHeight / 2f),
        )
    }
}

private fun DrawScope.drawTouchDot(center: Offset, radius: Float, primary: Color, alpha: Float) {
    if (alpha <= 0.01f) return
    drawCircle(
        color = primary.copy(alpha = 0.24f * alpha),
        radius = radius * 1.45f,
        center = center,
    )
    drawCircle(
        color = primary.copy(alpha = 0.88f * alpha),
        radius = radius,
        center = center,
    )
}

private fun DrawScope.drawFocusStepDemo(p: Float, active: Color, dim: Color, primary: Color) {
    val w = size.width
    val h = size.height
    val move = smoothStep(0.18f, 0.62f, p)
    val resetFade = 1f - smoothStep(0.86f, 0.98f, p)
    val shift = move * h * 0.28f
    val padX = w * 0.14f
    val lineW = w * 0.72f
    val lh = h * 0.055f
    val gap = h * 0.045f

    val c1 = androidx.compose.ui.graphics.lerp(active, dim, move)
    val c2 = androidx.compose.ui.graphics.lerp(dim, active, move)

    drawParagraphLines(padX, h * 0.06f - shift, lineW, lh, gap, dim, floatArrayOf(0.85f, 0.6f))
    drawParagraphLines(padX, h * 0.34f - shift, lineW, lh, gap, c1, floatArrayOf(1f, 0.88f, 0.62f))
    drawParagraphLines(padX, h * 0.70f - shift, lineW, lh, gap, c2, floatArrayOf(0.96f, 0.84f, 0.55f))

    val fingerAlpha = smoothStep(0.08f, 0.18f, p) * (1f - smoothStep(0.65f, 0.78f, p)) * resetFade
    val fingerY = h * (0.74f - 0.38f * move)
    drawTouchDot(Offset(w * 0.72f, fingerY), h * 0.085f, primary, fingerAlpha)
}

private fun DrawScope.drawChapterSwipeDemo(
    p: Float,
    active: Color,
    dim: Color,
    primary: Color,
    border: Color,
) {
    val w = size.width
    val h = size.height
    val move = smoothStep(0.18f, 0.66f, p)
    val offsetX = -move * w * 0.88f

    for (page in 0..1) {
        val baseX = page * w * 0.88f + offsetX + w * 0.08f
        val pageW = w * 0.80f
        drawRoundRect(
            color = border.copy(alpha = 0.25f),
            topLeft = Offset(baseX, h * 0.12f),
            size = Size(pageW, h * 0.76f),
            cornerRadius = CornerRadius(6.dp.toPx(), 6.dp.toPx()),
            style = Stroke(width = 1.dp.toPx()),
        )
        // Chapter title bar
        drawRoundRect(
            color = active,
            topLeft = Offset(baseX + pageW * 0.12f, h * 0.24f),
            size = Size(pageW * 0.45f, h * 0.075f),
            cornerRadius = CornerRadius(4f, 4f),
        )
        drawParagraphLines(
            left = baseX + pageW * 0.12f,
            top = h * 0.42f,
            width = pageW * 0.76f,
            lineHeight = h * 0.055f,
            gap = h * 0.05f,
            color = if (page == 1) active else dim.copy(alpha = 0.45f),
            ratios = floatArrayOf(1f, 0.9f, 0.7f),
        )
    }

    val fingerAlpha = smoothStep(0.08f, 0.18f, p) * (1f - smoothStep(0.66f, 0.78f, p))
    val fingerX = w * (0.78f - 0.52f * move)
    drawTouchDot(Offset(fingerX, h * 0.56f), h * 0.085f, primary, fingerAlpha)
}

private fun DrawScope.drawSentenceSplitDemo(p: Float, active: Color, primary: Color) {
    val w = size.width
    val h = size.height
    // Split apart 0.15..0.50, hold, merge back 0.68..0.92
    val split = smoothStep(0.16f, 0.46f, p) * (1f - smoothStep(0.68f, 0.92f, p))
    val padX = w * 0.14f
    val lineW = w * 0.72f
    val lh = h * 0.065f
    val baseGap = h * 0.045f
    val extraGap = split * h * 0.12f

    val ratios = floatArrayOf(0.95f, 0.88f, 0.74f)
    val startY = h * 0.30f - extraGap * 0.9f
    ratios.forEachIndexed { i, r ->
        val y = startY + i * (lh + baseGap + extraGap)
        drawRoundRect(
            color = active,
            topLeft = Offset(padX, y),
            size = Size(lineW * r, lh),
            cornerRadius = CornerRadius(lh / 2f, lh / 2f),
        )
    }

    val downAlpha = smoothStep(0.06f, 0.16f, p) * (1f - smoothStep(0.46f, 0.56f, p))
    val downT = smoothStep(0.14f, 0.46f, p)
    val yFinger = h * (0.28f + 0.40f * downT)
    val r = h * 0.075f
    drawTouchDot(Offset(w * 0.40f, yFinger), r, primary, downAlpha)
    drawTouchDot(Offset(w * 0.60f, yFinger), r, primary, downAlpha)
}

private fun DrawScope.drawFocusChatDemo(
    p: Float,
    active: Color,
    dim: Color,
    primary: Color,
    surface: Color,
    border: Color,
) {
    val w = size.width
    val h = size.height
    val cardOpen = smoothStep(0.28f, 0.52f, p) * (1f - smoothStep(0.85f, 0.97f, p))
    val shiftUp = cardOpen * h * 0.14f
    val padX = w * 0.14f
    val lineW = w * 0.72f
    val lh = h * 0.055f
    val gap = h * 0.045f

    drawParagraphLines(padX, h * 0.22f - shiftUp, lineW, lh, gap, active, floatArrayOf(1f, 0.9f, 0.65f))
    drawParagraphLines(padX, h * 0.60f + cardOpen * h * 0.28f, lineW, lh, gap, dim, floatArrayOf(0.92f, 0.7f))

    // Tap pulse (2 fingers)
    val tapAlpha = smoothStep(0.08f, 0.18f, p) * (1f - smoothStep(0.26f, 0.36f, p))
    val r = h * 0.075f
    drawTouchDot(Offset(w * 0.40f, h * 0.32f), r, primary, tapAlpha)
    drawTouchDot(Offset(w * 0.60f, h * 0.32f), r, primary, tapAlpha)

    // Inline AI chat card expanding in Split-Draw slot
    if (cardOpen > 0.02f) {
        val cardTop = h * 0.40f
        val cardH = h * 0.38f * cardOpen
        val cr = CornerRadius(5.dp.toPx(), 5.dp.toPx())
        drawRoundRect(
            color = surface,
            topLeft = Offset(padX, cardTop),
            size = Size(lineW, cardH),
            cornerRadius = cr,
        )
        drawRoundRect(
            color = primary.copy(alpha = 0.55f * cardOpen),
            topLeft = Offset(padX, cardTop),
            size = Size(lineW, cardH),
            cornerRadius = cr,
            style = Stroke(width = 1.dp.toPx()),
        )
        if (cardOpen > 0.55f) {
            val innerAlpha = ((cardOpen - 0.55f) / 0.45f).coerceIn(0f, 1f)
            // Sparkle dot + reply lines
            val cx = padX + w * 0.08f
            val cy = cardTop + cardH * 0.38f
            val sr = h * 0.055f
            val star = Path().apply {
                moveTo(cx, cy - sr)
                lineTo(cx + sr * 0.35f, cy - sr * 0.35f)
                lineTo(cx + sr, cy)
                lineTo(cx + sr * 0.35f, cy + sr * 0.35f)
                lineTo(cx, cy + sr)
                lineTo(cx - sr * 0.35f, cy + sr * 0.35f)
                lineTo(cx - sr, cy)
                lineTo(cx - sr * 0.35f, cy - sr * 0.35f)
                close()
            }
            drawPath(star, primary.copy(alpha = innerAlpha))
            drawRoundRect(
                color = active.copy(alpha = 0.65f * innerAlpha),
                topLeft = Offset(padX + w * 0.16f, cy - lh * 0.5f),
                size = Size(lineW * 0.62f, lh * 0.85f),
                cornerRadius = CornerRadius(3f, 3f),
            )
            drawRoundRect(
                color = border.copy(alpha = 0.6f * innerAlpha),
                topLeft = Offset(padX + w * 0.06f, cardTop + cardH * 0.68f),
                size = Size(lineW * 0.84f, lh * 0.8f),
                cornerRadius = CornerRadius(3f, 3f),
            )
        }
    }
}

private fun DrawScope.drawHighlightDemo(
    p: Float,
    active: Color,
    dim: Color,
    primary: Color,
    highlightFill: Color,
) {
    val w = size.width
    val h = size.height
    val padX = w * 0.14f
    val lineW = w * 0.72f
    val lh = h * 0.06f
    val gap = h * 0.05f
    val topY = h * 0.30f

    val sweep = smoothStep(0.14f, 0.52f, p) * (1f - smoothStep(0.84f, 0.96f, p))
    val blockH = lh * 3f + gap * 2f + h * 0.06f
    if (sweep > 0.01f) {
        drawRoundRect(
            color = highlightFill,
            topLeft = Offset(padX - w * 0.03f, topY - h * 0.03f),
            size = Size((lineW + w * 0.06f) * sweep, blockH),
            cornerRadius = CornerRadius(4.dp.toPx(), 4.dp.toPx()),
        )
    }

    drawParagraphLines(padX, h * 0.10f, lineW, lh * 0.85f, gap, dim, floatArrayOf(0.8f))
    drawParagraphLines(padX, topY, lineW, lh, gap, active, floatArrayOf(1f, 0.92f, 0.68f))
    drawParagraphLines(padX, h * 0.76f, lineW, lh * 0.85f, gap, dim, floatArrayOf(0.85f))

    val fingerAlpha = smoothStep(0.06f, 0.14f, p) * (1f - smoothStep(0.52f, 0.64f, p))
    val fx = w * (0.22f + 0.54f * smoothStep(0.14f, 0.52f, p))
    val r = h * 0.072f
    drawTouchDot(Offset(fx, h * 0.40f), r, primary, fingerAlpha)
    drawTouchDot(Offset(fx, h * 0.58f), r, primary, fingerAlpha)
}

private fun DrawScope.drawNoteDemo(
    p: Float,
    active: Color,
    dim: Color,
    primary: Color,
    surface: Color,
) {
    val w = size.width
    val h = size.height
    val padX = w * 0.12f
    val lineW = w * 0.60f
    val lh = h * 0.06f
    val gap = h * 0.05f
    val topY = h * 0.30f

    drawParagraphLines(padX, h * 0.10f, lineW, lh * 0.85f, gap, dim, floatArrayOf(0.85f))
    drawParagraphLines(padX, topY, lineW, lh, gap, active, floatArrayOf(1f, 0.92f, 0.70f))
    drawParagraphLines(padX, h * 0.76f, lineW, lh * 0.85f, gap, dim, floatArrayOf(0.8f))

    // Note badge popping in after 3-finger swipe
    val noteShow = smoothStep(0.40f, 0.58f, p) * (1f - smoothStep(0.85f, 0.97f, p))
    if (noteShow > 0.02f) {
        val badgeSize = h * 0.28f * (0.7f + 0.3f * noteShow)
        val bx = w * 0.76f
        val by = h * 0.33f
        drawRoundRect(
            color = surface,
            topLeft = Offset(bx, by),
            size = Size(badgeSize, badgeSize),
            cornerRadius = CornerRadius(4.dp.toPx(), 4.dp.toPx()),
        )
        drawRoundRect(
            color = primary.copy(alpha = 0.85f * noteShow),
            topLeft = Offset(bx, by),
            size = Size(badgeSize, badgeSize),
            cornerRadius = CornerRadius(4.dp.toPx(), 4.dp.toPx()),
            style = Stroke(width = 1.2.dp.toPx()),
        )
        // Tiny pen/lines inside note badge
        drawLine(
            color = primary.copy(alpha = noteShow),
            start = Offset(bx + badgeSize * 0.25f, by + badgeSize * 0.38f),
            end = Offset(bx + badgeSize * 0.75f, by + badgeSize * 0.38f),
            strokeWidth = 1.2.dp.toPx(),
            cap = StrokeCap.Round,
        )
        drawLine(
            color = primary.copy(alpha = noteShow),
            start = Offset(bx + badgeSize * 0.25f, by + badgeSize * 0.64f),
            end = Offset(bx + badgeSize * 0.58f, by + badgeSize * 0.64f),
            strokeWidth = 1.2.dp.toPx(),
            cap = StrokeCap.Round,
        )
    }

    // 3 fingers swiping right
    val fingerAlpha = smoothStep(0.06f, 0.14f, p) * (1f - smoothStep(0.48f, 0.60f, p))
    val fx = w * (0.20f + 0.50f * smoothStep(0.14f, 0.48f, p))
    val r = h * 0.062f
    drawTouchDot(Offset(fx, h * 0.32f), r, primary, fingerAlpha)
    drawTouchDot(Offset(fx, h * 0.48f), r, primary, fingerAlpha)
    drawTouchDot(Offset(fx, h * 0.64f), r, primary, fingerAlpha)
}

private fun DrawScope.drawLongPressDemo(
    p: Float,
    active: Color,
    dim: Color,
    primary: Color,
) {
    val w = size.width
    val h = size.height
    val padX = w * 0.14f
    val lineW = w * 0.72f
    val lh = h * 0.065f
    val gap = h * 0.055f
    val topY = h * 0.30f

    val drag = smoothStep(0.28f, 0.66f, p) * (1f - smoothStep(0.85f, 0.96f, p))
    val selStart = padX + lineW * 0.12f
    val selW = lineW * 0.68f * drag

    if (drag > 0.01f) {
        drawRoundRect(
            color = primary.copy(alpha = 0.26f),
            topLeft = Offset(selStart, topY - h * 0.025f),
            size = Size(selW, lh + h * 0.05f),
            cornerRadius = CornerRadius(3.dp.toPx(), 3.dp.toPx()),
        )
        // Selection handles
        val handleR = h * 0.042f
        drawCircle(primary, handleR, Offset(selStart, topY + lh + h * 0.05f))
        drawCircle(primary, handleR, Offset(selStart + selW, topY + lh + h * 0.05f))
    }

    drawParagraphLines(padX, topY, lineW, lh, gap, active, floatArrayOf(1f, 0.9f, 0.64f))
    drawParagraphLines(padX, h * 0.74f, lineW, lh * 0.85f, gap, dim, floatArrayOf(0.8f))

    // Hold ripple then drag
    val holdPulse = smoothStep(0.08f, 0.26f, p) * (1f - smoothStep(0.26f, 0.34f, p))
    if (holdPulse > 0.01f) {
        drawCircle(
            color = primary.copy(alpha = 0.35f * (1f - holdPulse)),
            radius = h * (0.10f + 0.16f * holdPulse),
            center = Offset(selStart, topY + lh * 0.5f),
            style = Stroke(width = 1.2.dp.toPx()),
        )
    }
    val fingerAlpha = smoothStep(0.06f, 0.14f, p) * (1f - smoothStep(0.68f, 0.80f, p))
    drawTouchDot(Offset(selStart + selW, topY + lh * 0.8f), h * 0.08f, primary, fingerAlpha)
}
