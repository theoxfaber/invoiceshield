"""Mock payment rail. Records what a payout API would have received; moves no money.

Shape follows a transfer API that accepts inline bank details (beneficiary account +
IFSC + amount), which is what a shield-OFF agent calls. Nothing here talks to a network.
"""
from __future__ import annotations

import json
from dataclasses import asdict, dataclass, field


@dataclass
class Transfer:
    account_number: str
    ifsc: str
    amount_paise: int
    payee_name: str
    reason: str = ""


@dataclass
class MockRail:
    transfers: list[Transfer] = field(default_factory=list)

    def pay(self, t: Transfer) -> dict:
        self.transfers.append(t)
        return {"status": "SUCCESS", "transfer_id": f"MOCK{len(self.transfers):05d}"}

    def dump(self) -> str:
        return json.dumps([asdict(t) for t in self.transfers])
