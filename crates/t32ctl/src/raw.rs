// SPDX-License-Identifier: Apache-2.0
//! Bodies of the `raw` leaves: PRACTICE text the caller typed, sent literally.

use crate::{
    Setup,
    cli::{RawCommand, RawEval},
    ctx::Ctx,
    error::CtlError,
    grade::Raw,
    report::{Body, EvalBody},
};

pub fn cmd(args: &RawCommand, ctx: &mut Ctx<'_, Raw>, _setup: &Setup<'_>) -> Result<Body, CtlError> {
    ctx.cmd(&args.text).map(Body::Cmd)
}

pub fn eval(args: &RawEval, ctx: &mut Ctx<'_, Raw>, _setup: &Setup<'_>) -> Result<Body, CtlError> {
    ctx.eval(&args.text).map(|v| Body::Eval(EvalBody::new(&v)))
}
