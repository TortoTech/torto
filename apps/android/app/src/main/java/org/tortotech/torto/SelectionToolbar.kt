package org.tortotech.torto

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Surface
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Popup
import androidx.compose.ui.window.PopupProperties
import org.tortotech.torto.core.SelectedText

/** The old app's compact icon surface, positioned in the same plane as native selection bounds. */
@Composable internal fun BoxScope.SelectionToolbar(
    selection: SelectedText,
    onCopy: () -> Unit,
    onDismiss: () -> Unit,
    onHighlight: (() -> Unit)? = null,
    onNote: (() -> Unit)? = null,
    onAsk: (() -> Unit)? = null,
    outsideClip: Boolean = false,
) {
    BoxWithConstraints(Modifier.matchParentSize()) {
        val anchor = selection.bounds.firstOrNull {
            it.y + it.height > 0 && it.y < maxHeight.value && it.x + it.width > 0 && it.x < maxWidth.value
        } ?: return@BoxWithConstraints
        val count = 2 + listOf(onHighlight, onNote, onAsk).count { it != null }
        val width = (count * 48 + 8).dp
        val left = (anchor.x + anchor.width / 2 - width.value / 2).dp
            .coerceIn(8.dp, (maxWidth - width - 8.dp).coerceAtLeast(8.dp))
        val last = selection.bounds.lastOrNull { it.y < maxHeight.value } ?: anchor
        val above = anchor.y.dp - 56.dp
        val below = (last.y + last.height).dp + 12.dp
        val top = if (outsideClip) above else
            (if (above < 8.dp && below + 48.dp <= maxHeight - 8.dp) below else above)
                .coerceIn(8.dp, (maxHeight - 56.dp).coerceAtLeast(8.dp))
        val surface: @Composable (Modifier) -> Unit = { modifier ->
            Surface(
                modifier = modifier,
                shape = RoundedCornerShape(12.dp),
                shadowElevation = 5.dp,
            ) {
                Row(Modifier.padding(horizontal = 4.dp)) {
                    ActionIcon("copy", tr("复制", "Copy"), onClick = onCopy)
                    onHighlight?.let { ActionIcon("highlight", tr("高亮", "Highlight"), onClick = it) }
                    onNote?.let { ActionIcon("edit-note", tr("批注", "Note"), onClick = it) }
                    onAsk?.let { ActionIcon("chat", tr("提问", "Ask"), onClick = it) }
                    ActionIcon("close", tr("取消选择", "Clear selection"), onClick = onDismiss)
                }
            }
        }
        // A short note can be shorter than the toolbar itself. Its lazy list
        // must not clip the copy/close targets or grow the note to fit them.
        if (outsideClip) {
            val density = LocalDensity.current
            Popup(
                alignment = Alignment.TopStart,
                offset = with(density) { IntOffset(left.roundToPx(), top.roundToPx()) },
                properties = PopupProperties(focusable = false, dismissOnClickOutside = false),
            ) { surface(Modifier) }
        } else surface(Modifier.offset(left, top))
    }
}
