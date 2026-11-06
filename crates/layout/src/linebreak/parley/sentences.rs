//! Optimize each forced sentence independently while retaining authored breaks,
//! source offsets, inline boxes and the existing leading-sentence geometry.
use super::*;

#[allow(clippy::too_many_arguments)]
pub(crate) fn plan_sentences(
    layout: &mut Layout<TextBrush>, text: &str, width: f32, first_indent: f32,
    continuation_indent: f32, em: f32, hyphens: &HashMap<usize, f32>,
    repeat_indent: bool, reference: Option<(&Layout<TextBrush>, &str)>,
) -> Option<ReusableParagraphPlan> {
    if layout.is_rtl() || !text.contains('\n') { return None; }
    let prefix = reference.and_then(|reference| {
        let mut wrapped=layout.clone();
        let mut plan=plan_wrapped_with_sentence_prefix(&mut wrapped,text,width,Some(reference),true)?;
        let count=wrapped.lines().position(|line| line.break_reason()==parley::layout::BreakReason::Explicit)?+1;
        let end=text.find('\n')?;
        plan.lines.truncate(count);
        plan.hyphen_offsets.truncate(count);
        plan.adjustments.retain(|a| a.range.end<=end);
        Some(plan)
    });
    let clusters=measured_prose_clusters(layout,true)?;
    // Out-of-flow zero-size boxes are internal shaping fences added at
    // selected hyphens. They consume no text/line items and must survive
    // re-planning after a ligature or fallback run changes its measurement.
    let boxes=layout.inline_boxes().filter(|b|b.kind==InlineBoxKind::InFlow).collect::<Vec<_>>();
    let mut result=ParagraphPlan {lines:Vec::new(),adjustments:Vec::new(),hyphen_offsets:Vec::new()};
    let mut start=0;
    let mut item_offset=0;
    for part in text.split_inclusive('\n') {
        let hard=part.ends_with('\n');
        let body=part.strip_suffix('\n').unwrap_or(part);
        let end=start+body.len();
        // Empty/authored control lines retain the established wrapping fallback.
        if body.is_empty() { return None; }
        let measured=clusters.iter().filter(|c| c.range.start>=start && c.range.end<=end)
            .cloned().map(|mut c| {c.range=c.range.start-start..c.range.end-start;c}).collect::<Vec<_>>();
        let inline=boxes.iter().filter(|b| b.index>=start && b.index<=end)
            .cloned().map(|mut b| {b.index-=start;b}).collect::<Vec<_>>();
        let candidates=hyphens.iter().filter(|(p,_)| **p>start && **p<=end)
            .map(|(p,w)| (*p-start,*w)).collect::<HashMap<_,_>>();
        let mut plan=if start==0 && prefix.is_some() {
            // Prefix counts already include the explicit newline.
            let plan=prefix.as_ref()?;
            let expected=measured.len()+inline.len()+usize::from(hard);
            if plan.lines.iter().map(|l|l.cluster_count as usize).sum::<usize>()!=expected {return None;}
            result.lines.extend_from_slice(&plan.lines);
            result.adjustments.extend(plan.adjustments.iter().cloned());
            result.hyphen_offsets.extend_from_slice(&plan.hyphen_offsets);
            item_offset=plan.lines.last()?.breakpoint;
            start+=part.len();
            continue;
        } else {
            plan_measured_content_with_analysis(body,&measured,&inline,width,
                if start==0 || repeat_indent {first_indent} else {continuation_indent},
                continuation_indent,em,&candidates,&ParagraphAnalysis::new(body))?
        };
        if hard {
            // Parley consumes the newline as an item; never paint a hyphen there.
            let last=plan.lines.last_mut()?;
            last.cluster_count+=1;
            last.breakpoint+=1;
            last.hyphenated=false;
            *plan.hyphen_offsets.last_mut()?=None;
        }
        for line in &mut plan.lines {line.breakpoint+=item_offset;}
        item_offset=plan.lines.last()?.breakpoint;
        for a in &mut plan.adjustments {a.range=a.range.start+start..a.range.end+start;}
        result.lines.extend(plan.lines);
        result.adjustments.extend(plan.adjustments);
        result.hyphen_offsets.extend(plan.hyphen_offsets.into_iter().map(|p|p.map(|p|p+start)));
        start+=part.len();
    }
    Some(ReusableParagraphPlan {plan:result,clusters,explicit:true})
}
