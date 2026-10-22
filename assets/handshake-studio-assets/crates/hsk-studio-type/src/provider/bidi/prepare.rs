use core::{cmp::max, ops::Range};
use super::{BidiClass::{self,*}, level::Level};
use crate::{Error, provider::{Context, ProviderResult, storage::ChargedVec}};
pub type LevelRun = Range<usize>;
pub type LevelRunVec<'a> = ChargedVec<'a,LevelRun>;
pub struct IsolatingRunSequence<'a> {
    pub runs: LevelRunVec<'a>,
    pub sos: BidiClass,
    pub eos: BidiClass,
}
pub type IsolatingRunSequenceVec<'a> = ChargedVec<'a,IsolatingRunSequence<'a>>;
pub fn removed_by_x9(class:BidiClass)->bool { matches!(class,RLE|LRE|RLO|LRO|PDF|BN) }
pub fn not_removed_by_x9(class:&BidiClass)->bool { !removed_by_x9(*class) }
impl IsolatingRunSequence<'_> {
    pub(crate) fn iter_forwards_from(&self,pos:usize,index:usize)->impl Iterator<Item=usize>+'_ {
        let runs=&self.runs[index..];
        (pos..runs[0].end).chain(runs[1..].iter().flat_map(Clone::clone))
    }
    pub(crate) fn iter_backwards_from(&self,pos:usize,index:usize)->impl Iterator<Item=usize>+'_ {
        (self.runs[index].start..pos).rev().chain(self.runs[..index].iter().rev().flat_map(Clone::clone))
    }
}
fn find_index<'a>(iter:impl Iterator<Item=usize>,classes:&[BidiClass],ctx:&Context<'a>)->ProviderResult<Option<usize>> {
    for index in iter { ctx.step()?; if not_removed_by_x9(&classes[index]) { return Ok(Some(index)); } }
    Ok(None)
}
/// X10/BD13; borrowed ranges and admitted nested storage, no owned clones.
pub fn isolating_run_sequences<'a>(para_level:Level,classes:&[BidiClass],levels:&[Level],
    runs:LevelRunVec<'a>,_has_isolates:bool,out:&mut IsolatingRunSequenceVec<'a>,ctx:&Context<'a>)->ProviderResult<()> {
    if classes.len()!=levels.len() { return Err(Error::InvalidBidi); }
    let mut sequences=ChargedVec::with_capacity(runs.len(),ctx)?;
    let mut stack=ChargedVec::new();
    stack.push(LevelRunVec::new(),ctx)?;
    for run in runs.iter() {
        ctx.step()?;
        if run.is_empty() || run.end>classes.len() || stack.is_empty() { return Err(Error::InvalidBidi); }
        let start_class=classes[run.start];
        let end_index=find_index((run.start..run.end).rev(),classes,ctx)?;
        let end_class=end_index.map(|i| classes[i]).unwrap_or(start_class);
        let mut sequence=if start_class==PDI && stack.len()>1 {
            stack.pop().ok_or(Error::InvalidBidi)?
        } else { LevelRunVec::new() };
        sequence.push(run.clone(),ctx)?;
        if matches!(end_class,RLI|LRI|FSI) { stack.push(sequence,ctx)?; }
        else { sequences.push(sequence,ctx)?; }
    }
    while !stack.is_empty() {
        ctx.step()?;
        let sequence=stack.pop().ok_or(Error::InvalidBidi)?;
        if !sequence.is_empty() { sequences.push(sequence,ctx)?; }
    }
    for sequence in sequences.as_mut_slice() {
        ctx.step()?;
        let start=sequence[0].start;
        let end=sequence[sequence.len()-1].end;
        let first=find_index(sequence.iter().flat_map(Clone::clone),classes,ctx)?.unwrap_or(start);
        let last=find_index(sequence.iter().rev().flat_map(|r|r.clone().rev()),classes,ctx)?.unwrap_or(end-1);
        let predecessor=find_index((0..start).rev(),classes,ctx)?.map(|i|levels[i]).unwrap_or(para_level);
        let last_non_removed=find_index((0..end).rev(),classes,ctx)?.map(|i|classes[i]).unwrap_or(BN);
        let successor=if matches!(last_non_removed,RLI|LRI|FSI) { para_level }
            else { find_index(end..classes.len(),classes,ctx)?.map(|i|levels[i]).unwrap_or(para_level) };
        let owned=core::mem::take(sequence);
        out.push(IsolatingRunSequence { runs:owned,sos:max(levels[first],predecessor).bidi_class(),eos:max(levels[last],successor).bidi_class() },ctx)?;
    }
    Ok(())
}
