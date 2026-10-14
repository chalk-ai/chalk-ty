from __future__ import annotations

import json

from chalk import online


def build_dict(values: list[int]):
    return {value: value for value in values}


def intermediate(values: list[int]):
    return build_dict(values)


@online
def typed_list(values: list[int]):
    return {value: value for value in values}


@online
def constant_list(values: list[int]):
    return {value: value for value in [1, 2]}


@online
def empty_list(values: list[int]):
    return {value: value for value in []}


@online
def constant_tuple(values: list[int]):
    return {value: value for value in (1, 2)}


@online
def filtered(values: list[int]):
    return {value: value for value in values if value > 0}


@online
def false_filter(values: list[int]):
    return {value: value for value in values if False}


@online
def multiple_generators(values: list[int]):
    return {left: right for left in values for right in values}


@online
def nested_value(values: list[int]):
    return {value: {other: other for other in values} for value in values}


@online
def local_assignment(values: list[int]):
    result = {value: value for value in values}
    return result


@online
def unused_assignment(values: list[int]):
    _result = {value: value for value in values}
    return 1


@online
def passed_to_len(values: list[int]):
    return len({value: value for value in values})


@online
def json_input(values: list[int]):
    return {value: value for value in json.loads("[1, 2]")}


@online
def helper_call(values: list[int]):
    return build_dict(values)


@online
def transitive_helper(values: list[int]):
    return intermediate(values)


@online
def constant_dead_branch(values: list[int]):
    if False:
        return {value: value for value in values}
    return 1


@online
def after_return(values: list[int]):
    return 1
    _result = {value: value for value in values}


@online
def runtime_branch(values: list[int]):
    if len(values) > 0:
        return {value: value for value in values}
    return {}


@online
def dict_literal_control(values: list[int]):
    return {1: 2}


@online
def empty_dict_control(values: list[int]):
    return {}


@online
def list_comprehension_control(values: list[int]):
    return [value for value in values]  # noqa: C416 -- Probe list-comprehension support.


@online
def unused_helper_control(values: list[int]):
    return 1


@online
def after_both_branches_return(values: list[int]):
    if len(values) > 0:
        return 1
    else:
        return 2
    return {value: value for value in values}


@online
def after_one_branch_returns(values: list[int]):
    if len(values) > 0:
        return 1
    return {value: value for value in values}


@online
def after_constant_true_return(values: list[int]):
    if True:
        return 1
    return {value: value for value in values}


@online
def after_constant_false_return(values: list[int]):
    if False:
        return 1
    return {value: value for value in values}


@online
def return_then_comp_in_branch(values: list[int]):
    if len(values) > 0:
        return 1
        _result = {value: value for value in values}
    return 2


@online
def after_raise(values: list[int]):
    raise ValueError("stop")
    return {value: value for value in values}


@online
def raise_control(values: list[int]):
    raise ValueError("stop")


@online
def after_assert_false(values: list[int]):
    assert False
    return {value: value for value in values}


@online
def assert_false_control(values: list[int]):
    assert False
    return 1


@online
def after_break(values: list[int]):
    for _item in values:
        break
        _result = {value: value for value in values}
    return 1


@online
def break_control(values: list[int]):
    for _item in values:
        break
    return 1


@online
def after_continue(values: list[int]):
    for _item in values:
        continue
        _result = {value: value for value in values}
    return 1


@online
def continue_control(values: list[int]):
    for _item in values:
        continue
    return 1


@online
def after_loop_return(values: list[int]):
    for _item in values:
        return 1
    return {value: value for value in values}


@online
def empty_loop_body(values: list[int]):
    for _item in []:
        _result = {value: value for value in values}
    return 1


@online
def while_false_body(values: list[int]):
    while False:
        _result = {value: value for value in values}
    return 1


@online
def while_false_control(values: list[int]):
    while False:
        pass
    return 1


@online
def after_try_finally_return(values: list[int]):
    try:
        return 1
    finally:
        pass
    return {value: value for value in values}


@online
def try_finally_control(values: list[int]):
    try:
        return 1
    finally:
        pass


@online
def dead_helper_call(values: list[int]):
    return 1
    _result = build_dict(values)


@online
def conditional_dead_helper_call(values: list[int]):
    if False:
        return build_dict(values)
    return 1


@online
def after_bare_raise(values: list[int]):
    raise
    return {value: value for value in values}


@online
def bare_raise_control(values: list[int]):
    raise


@online
def empty_loop_control(values: list[int]):
    for _item in []:
        pass
    return 1
