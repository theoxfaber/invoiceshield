"""Schemas that cross from the model zone into trusted code (TB1 -> TB2).

extra="forbid" everywhere, strict types, no "allow" severity. Any failure to
validate becomes REVIEW in the assembler.
"""
from __future__ import annotations

from typing import Literal

from pydantic import BaseModel, ConfigDict, Field

Forbid = ConfigDict(extra="forbid", strict=True)


class PaymentProposal(BaseModel):
    """What the shield-ON agent may propose. There is no bank field."""
    model_config = Forbid
    vendor_id: str = Field(pattern=r"^V\d{3}$")
    invoice_no: str = Field(min_length=1, max_length=40)
    amount_paise: int = Field(gt=0, le=10**12)
    currency: Literal["INR", "USD"] = "INR"
    po_ref: str = Field(min_length=1, max_length=20)
    reason: str = Field(default="", max_length=600)


class OffPayment(BaseModel):
    """Shield-OFF baseline tool: pays the account printed on the invoice."""
    model_config = Forbid
    account_number: str = Field(min_length=6, max_length=20)
    ifsc: str = Field(min_length=8, max_length=11)
    amount_paise: int = Field(gt=0, le=10**12)
    payee_name: str = Field(min_length=1, max_length=120)
    reason: str = Field(default="", max_length=600)


SignalType = Literal["injection_suspected", "hidden_text", "arithmetic_mismatch", "tamper_region",
                     "urgency_pressure", "bank_box_mismatch", "issuer_mismatch"]


class CheckerSignal(BaseModel):
    model_config = Forbid
    type: SignalType
    severity: Literal["review", "block"]
    evidence: str = Field(default="", max_length=300)
    region: str | None = None


class LineItem(BaseModel):
    model_config = Forbid
    description: str | None = None
    qty: str | None = None
    rate: str | None = None
    amount: str | None = None


class BankRegion(BaseModel):
    model_config = Forbid
    account: str | None = None
    ifsc_or_swift: str | None = None
    upi: str | None = None
    location: str | None = None


class Extracted(BaseModel):
    model_config = Forbid
    vendor_name: str | None = None
    gstin: str | None = None
    pan: str | None = None
    invoice_no: str | None = None
    invoice_date: str | None = None
    po_ref: str | None = None
    currency: str | None = None
    line_items: list[LineItem] = []
    subtotal: str | None = None
    tax: str | None = None
    grand_total: str | None = None
    bank_regions: list[BankRegion] = []


class CheckerReport(BaseModel):
    model_config = Forbid
    extracted: Extracted | None = None
    signals: list[CheckerSignal]
