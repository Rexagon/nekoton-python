import asyncio
import os

from nekoton import *

dirname = os.path.dirname(__file__)

# Address
assert(not Address.validate("totally invalid address"))
assert(Address.validate("0:a921453472366b7feeec15323a96b5dcf17197c88dc0d4578dfa52900b8a33cb"))
my_addr = Address('-1:a921453472366b7feeec15323a96b5dcf17197c88dc0d4578dfa52900b8a33cb')
assert(my_addr.workchain == -1 and len(my_addr.account) == 32)
my_addr.workchain = 0
assert(my_addr.workchain == 0)

# Cells
cell1 = Cell()
assert(len(Cell().repr_hash) == 32)
assert(Cell() == Cell())

complex_cell_abi = [
    ("first", AbiUint(32)),
    ("second", AbiBool()),
]
complex_cell = Cell.build(
    abi=complex_cell_abi,
    value={
        "first": 123,
        "second": True,
    }
)
assert(Cell.decode(complex_cell.encode('base64'), 'base64') == complex_cell)
assert(Cell.from_bytes(complex_cell.to_bytes()) == complex_cell)

decoded = complex_cell.unpack(abi=complex_cell_abi)
assert(decoded["first"] == 123)
assert(decoded["second"] == True)

# Crypto
seed = Bip39Seed.generate()
assert(seed.word_count == 24)

keypair0 = seed.derive()
assert(len(keypair0.public_key.to_bytes()) == 32)
assert(seed.derive() == seed.derive(path=Bip39Seed.path_for_account(0)))

keypair1 = seed.derive(path=Bip39Seed.path_for_account(1))
assert(keypair0 != keypair1)
assert(len(keypair1.public_key.encode('hex')) == 64)

# Abi
with open(os.path.join(dirname, 'wallet.abi.json'), 'r') as json:
    abi = ContractAbi(json.read())

assert(abi.abi_version == AbiVersion(2, 3))
assert(abi.get_function("non-existing") is None)
assert(abi.get_event("non-existing") is None)

send_transaction_func = abi.get_function("sendTransaction")

send_transaction_input = {
    "dest": my_addr,
    "value": 123,
    "bounce": False,
    "flags": 3,
    "payload": Cell(),
}
body_cell = send_transaction_func.encode_internal_input(send_transaction_input)
assert(body_cell != Cell())

internal_msg = send_transaction_func.encode_internal_message(
    data=send_transaction_input,
    value=13213123,
    bounce=False,
    dst=my_addr,
)
assert(internal_msg.state_init is None)
assert(internal_msg.body == body_cell)

unpacked_body = body_cell.unpack(abi=[
    ("function_id", AbiUint(32)),
    ("dest", AbiAddress()),
    ("value", AbiUint(128)),
    ("bounce", AbiBool()),
    ("flags", AbiUint(8)),
    ("payload", AbiCell()),
])
decoded_body = send_transaction_func.decode_input(message_body=body_cell, internal=True)

for field, item in send_transaction_input.items():
    assert(unpacked_body[field] == item)
    assert(decoded_body[field] == item)

unsigned_body = send_transaction_func.encode_external_input(send_transaction_input, address=my_addr)
assert(unsigned_body.sign(keypair0) == unsigned_body.with_signature(keypair0.sign(unsigned_body.hash)))

unsigned_message = send_transaction_func.encode_external_message(my_addr, send_transaction_input)
assert(len(unsigned_message.without_signature().hash))

# Subscriptions
async def main():
    clock = Clock()

    transport = JrpcTransport(endpoint="https://jrpc.everwallet.net")
    await transport.check_connection()
    signature_id = await transport.get_signature_id()
    assert(signature_id is None)

    subscription = await transport.subscribe(my_addr)


if __name__ == "__main__":
    asyncio.run(main())
    
